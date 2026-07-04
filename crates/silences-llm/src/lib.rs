//! DeepSeek 流式 API 调用
//!
//! 使用 OpenAI-compatible `/v1/chat/completions` 端点。
//! 流式模式 + `reasoning_effort` + `include_usage`
//!
//! 参考 CodeWhale 的实践处理 DeepSeek v4 的流式响应格式。

use std::collections::HashSet;
use std::path::PathBuf;
use std::pin::Pin;
use std::sync::{Arc, RwLock};

use anyhow::{Context, Result};
use bytes::Bytes;
use futures_util::StreamExt;
use reqwest::header::{AUTHORIZATION, CONTENT_TYPE};
use serde_json::{Value, json};
use silences_core::{Message, TokenUsage};
use tokenizers::Tokenizer;

type ByteStream = Pin<Box<dyn futures_util::Stream<Item = reqwest::Result<Bytes>> + Send>>;

/// DeepSeek LLM 客户端
#[derive(Clone)]
pub struct LlmClient {
    http: reqwest::Client,
    api_key: Arc<RwLock<String>>,
    base_url: String,
    model: String,
    tokenizer: Option<Tokenizer>,
    /// API 调试日志目录（设为 Some 后会将每次请求体写入该目录）
    debug_dir: Option<PathBuf>,
}

impl LlmClient {
    pub fn new(api_key: String, base_url: String, model: String) -> Self {
        let http = reqwest::Client::builder()
            .build()
            .expect("Failed to create HTTP client");
        Self { http, api_key: Arc::new(RwLock::new(api_key)), base_url, model, tokenizer: None, debug_dir: None }
    }

    /// 设置 API 调试日志目录
    ///
    /// 每次 API 请求体将以 JSON 格式记录到此目录的 `api_debug.json` 文件中，
    /// 只保留最近 100 条请求。
    pub fn with_debug_dir(mut self, dir: PathBuf) -> Self {
        self.debug_dir = Some(dir);
        self
    }

    /// 运行时更新 API key
    pub fn update_api_key(&self, new_key: String) {
        if let Ok(mut key) = self.api_key.write() {
            *key = new_key;
        }
    }

    /// 获取当前 API key 的克隆
    pub fn api_key_snapshot(&self) -> Option<String> {
        self.api_key.read().ok().map(|k| k.clone())
    }

    /// 为 agent 产生一个独立的克隆（每个 agent 任务拥有独立的 client）
    pub fn clone_for_agent(&self) -> Self {
        self.clone()
    }

    /// 发送 max_tokens=1 的预热请求，触发服务端计算并缓存 KV cache。
    ///
    /// 预热后，实际的 chat_stream 请求可以继承此请求的前缀缓存。
    /// 适用于：A + u_user + SILENCES.md 等跨轮稳定前缀。
    pub async fn warmup_prefix(
        &self,
        messages: &[Message],
        system: Option<&str>,
    ) -> Result<WarmupInfo> {
        let api_messages = Self::build_api_messages(messages, system);

        let body = json!({
            "model": self.model,
            "messages": api_messages,
            "stream": true,
            "stream_options": { "include_usage": true },
            "max_tokens": 1,
        });

        if let Some(ref dir) = self.debug_dir {
            log_api_request(dir, &body);
        }

        let url = format!("{}/v1/chat/completions", self.base_url.trim_end_matches('/'));
        let api_key = self.api_key.read().map(|k| k.clone()).unwrap_or_else(|_| String::new());
        let response = self
            .http
            .post(&url)
            .header(AUTHORIZATION, format!("Bearer {api_key}"))
            .header(CONTENT_TYPE, "application/json")
            .json(&body)
            .send()
            .await
            .context("预热请求失败")?;

        let status = response.status();
        if !status.is_success() {
            let text = response.text().await.unwrap_or_default();
            anyhow::bail!("预热请求失败 HTTP {status}: {text}");
        }

        // 流式读取并解析 usage
        let byte_stream = Box::pin(response.bytes_stream());
        let mut usage: Option<TokenUsage> = None;

        use futures_util::StreamExt;
        let mut stream = byte_stream;
        let mut line_buf = String::new();

        while let Some(chunk) = stream.next().await {
            let chunk = chunk?;
            let s = String::from_utf8_lossy(&chunk);
            line_buf.push_str(&s);

            while let Some(nl) = line_buf.find('\n') {
                let raw = line_buf[..nl].to_string();
                line_buf.drain(..=nl);
                let line = raw.trim();
                if line.is_empty() || !line.starts_with("data: ") { continue; }
                let json_str = line.strip_prefix("data: ").unwrap().trim();
                if json_str == "[DONE]" { break; }
                if let Ok(val) = serde_json::from_str::<serde_json::Value>(json_str) {
                    if let Some(u) = val.get("usage") {
                        if !u.is_null() {
                            usage = Some(parse_usage(u));
                        }
                    }
                }
            }
        }

        let info = WarmupInfo {
            total_tokens: usage.as_ref().map(|u| u.input_tokens).unwrap_or(0),
            cache_hit_tokens: usage.as_ref().map(|u| u.cache_hit_tokens).unwrap_or(0),
            cache_miss_tokens: usage.as_ref().map(|u| u.cache_miss_tokens).unwrap_or(0),
        };

        eprintln!("[warmup] {} tok (miss {})", info.total_tokens, info.cache_miss_tokens);
        Ok(info)
    }

    pub fn with_tokenizer(mut self, path: &str) -> Self {
        self.tokenizer = Tokenizer::from_file(path).ok();
        if self.tokenizer.is_none() {
            eprintln!("[llm] 警告: 无法加载 tokenizer {path}");
        }
        self
    }

    /// 计算 messages 文本的 token 数（精确 + 回退估算）
    #[allow(dead_code)]
    fn count_tokens(&self, messages: &[Message], system: Option<&str>) -> usize {
        let text = build_counting_text(messages, system);
        if let Some(ref tok) = self.tokenizer {
            if let Ok(enc) = tok.encode(text.as_str(), true) {
                return enc.len();
            }
        }
        // 回退：中文 ~1/2，英文 ~1/4，+50 JSON 开销
        let cjk = text.chars().filter(|&c| c as u32 > 0x7F).count();
        let ascii = text.chars().filter(|&c| c as u32 <= 0x7F).count();
        cjk / 2 + ascii / 4 + 50
    }

    /// 构建 API messages JSON（含工具调用）
    ///
    /// 自动清理孤立的 tool_calls：如果 assistant 消息中的某些 tool_call 在后续消息中
    /// 没有对应的 tool_result，则将这些 tool_call 从消息中移除，避免 API 返回 400 错误。
    fn build_api_messages(messages: &[Message], system: Option<&str>) -> Vec<Value> {
        // 第一遍：收集所有有对应 tool_result 的 tool_call_id
        let mut completed_ids: HashSet<&str> = HashSet::new();
        for msg in messages {
            if msg.role == "tool" {
                if let Some(ref tcid) = msg.tool_call_id {
                    completed_ids.insert(tcid.as_str());
                }
            }
        }

        let mut api = Vec::new();
        if let Some(sys) = system {
            api.push(json!({"role": "system", "content": sys}));
        }
        for msg in messages {
            // 检查此消息的 tool_calls 是否有孤立的（无对应 tool_result）
            let all_tc_orphaned = msg.tool_calls.as_ref().map_or(false, |tc| {
                tc.iter().all(|tc| !completed_ids.contains(tc.id.as_str()))
            });

            // 如果 assistant 消息的所有 tool_calls 都是孤立的且 content 为空，
            // 直接跳过这条消息 —— 留它在消息列表中只会让 LLM 困惑
            if msg.role == "assistant" && all_tc_orphaned && msg.content.is_empty() {
                continue;
            }

            let content_val = if msg.content.is_empty() && msg.tool_calls.is_some() && !all_tc_orphaned {
                Value::Null
            } else {
                json!(msg.content)
            };
            let mut m = json!({"role": msg.role, "content": content_val});
            if let Some(ref name) = msg.name {
                m["name"] = json!(name);
            }
            if let Some(ref tc) = msg.tool_calls {
                // 只保留有对应 tool_result 的 tool_call
                let matched: Vec<_> = tc.iter().filter(|tc| completed_ids.contains(tc.id.as_str())).collect();
                if !matched.is_empty() {
                    m["tool_calls"] = json!(matched);
                }
                // 如果全部没有对应 tool_result（all_tc_orphaned=true 但 content 非空），
                // 则不发送 tool_calls 字段，仅保留文本内容
            }
            if let Some(ref tcid) = msg.tool_call_id {
                m["tool_call_id"] = json!(tcid);
            }
            api.push(m);
        }
        api
    }

    /// 流式聊天调用
    pub async fn chat_stream(
        &self,
        messages: &[Message],
        system: Option<&str>,
        tools: Option<Vec<Value>>,
    ) -> Result<ChatStream> {
        let api_messages = Self::build_api_messages(messages, system);

        let mut body = json!({
            "model": self.model,
            "messages": api_messages,
            "stream": true,
            "stream_options": { "include_usage": true },
            "reasoning_effort": "high",
            "thinking": { "type": "enabled" },
        });
        if let Some(t) = tools {
            body["tools"] = json!(t);
        }

        // 调试日志：写请求体到文件，只保留最近 100 条
        if let Some(ref dir) = self.debug_dir {
            log_api_request(dir, &body);
        }

        let url = format!("{}/v1/chat/completions", self.base_url.trim_end_matches('/'));
        let api_key = self.api_key.read().map(|k| k.clone()).unwrap_or_else(|_| String::new());
        let response = self
            .http
            .post(&url)
            .header(AUTHORIZATION, format!("Bearer {api_key}"))
            .header(CONTENT_TYPE, "application/json")
            .json(&body)
            .send()
            .await
            .context("Failed to send chat request")?;

        let status = response.status();
        if !status.is_success() {
            let text = response.text().await.unwrap_or_default();
            anyhow::bail!("DeepSeek API error HTTP {status}: {text}");
        }

        let byte_stream = Box::pin(response.bytes_stream());
        Ok(ChatStream {
            byte_stream,
            line_buf: String::new(),
            usage: None,
            ended: false,
        })
    }
}


/// 把 messages + system 拼成一段文本用于 token 计数
#[allow(dead_code)]
fn build_counting_text(messages: &[Message], system: Option<&str>) -> String {
    let mut text = String::new();
    if let Some(s) = system {
        text.push_str("system: ");
        text.push_str(s);
        text.push('\n');
    }
    for msg in messages {
        text.push_str(&msg.role);
        text.push_str(": ");
        text.push_str(&msg.content);
        text.push('\n');
    }
    text
}

/// 流式消息片段
#[derive(Debug, Clone)]
pub enum StreamDelta {
    Reasoning(String),
    Text(String),
    /// tool call 流式片段（按 index 累加 arguments）
    ToolCall {
        index: usize,
        id: Option<String>,
        name: Option<String>,
        arguments: String,
    },
}

pub struct ChatStream {
    byte_stream: ByteStream,
    line_buf: String,
    usage: Option<TokenUsage>,
    ended: bool,
}

impl ChatStream {
    pub async fn next_delta(&mut self) -> Result<Option<StreamDelta>> {
        if self.ended {
            return Ok(None);
        }

        loop {
            while let Some(newline_pos) = self.line_buf.find('\n') {
                let raw_line: String = self.line_buf[..newline_pos].to_string();
                self.line_buf.drain(..=newline_pos);

                let line = raw_line.trim();
                if line.is_empty() {
                    continue;
                }

                let Some(json_str) = line.strip_prefix("data: ") else {
                    continue;
                };
                let json_str = json_str.trim();

                if json_str == "[DONE]" {
                    self.ended = true;
                    return Ok(None);
                }

                let Ok(val) = serde_json::from_str::<Value>(json_str) else {
                    continue;
                };

                if let Some(usage_val) = val.get("usage") {
                    if !usage_val.is_null() {
                        eprintln!("<<< usage: {}", usage_val);
                        self.usage = Some(parse_usage(usage_val));
                    }
                }

                if let Some(choices) = val.get("choices").and_then(Value::as_array) {
                    for choice in choices {
                        if let Some(delta) = choice.get("delta") {
                            if let Some(r) = delta.get("reasoning_content").and_then(Value::as_str) {
                                if !r.is_empty() {
                                    return Ok(Some(StreamDelta::Reasoning(r.to_string())));
                                }
                            }
                            if let Some(c) = delta.get("content").and_then(Value::as_str) {
                                if !c.is_empty() {
                                    return Ok(Some(StreamDelta::Text(c.to_string())));
                                }
                            }
                            if let Some(tc_array) = delta.get("tool_calls").and_then(Value::as_array) {
                                for tc in tc_array {
                                    let index = tc.get("index").and_then(Value::as_u64).unwrap_or(0) as usize;
                                    let id = tc.get("id").and_then(Value::as_str).map(String::from);
                                    let name = tc.get("function")
                                        .and_then(|f| f.get("name"))
                                        .and_then(Value::as_str)
                                        .map(String::from);
                                    let arguments = tc.get("function")
                                        .and_then(|f| f.get("arguments"))
                                        .and_then(Value::as_str)
                                        .unwrap_or("")
                                        .to_string();
                                    return Ok(Some(StreamDelta::ToolCall { index, id, name, arguments }));
                                }
                            }
                        }
                    }
                }
            }

            match self.byte_stream.next().await {
                Some(Ok(chunk)) => {
                    let s = String::from_utf8_lossy(&chunk);
                    self.line_buf.push_str(&s);
                }
                Some(Err(e)) => {
                    self.ended = true;
                    return Err(anyhow::anyhow!("Stream read error: {e}"));
                }
                None => {
                    self.ended = true;
                    return Ok(None);
                }
            }
        }
    }

    pub async fn next_text(&mut self) -> Result<Option<String>> {
        loop {
            match self.next_delta().await? {
                Some(StreamDelta::Text(t)) => return Ok(Some(t)),
                Some(StreamDelta::Reasoning(_)) => continue,
                Some(StreamDelta::ToolCall { .. }) => continue,
                None => return Ok(None),
            }
        }
    }

    pub fn take_usage(&mut self) -> Option<TokenUsage> {
        self.usage.take()
    }
}

/// 预热请求结果
#[derive(Debug, Clone)]
pub struct WarmupInfo {
    /// 总 prompt tokens
    pub total_tokens: u32,
    /// 命中的缓存 tokens（预热前已有缓存的部分）
    pub cache_hit_tokens: u32,
    /// 本次新计算并写入缓存的 tokens
    pub cache_miss_tokens: u32,
}

/// 从 API usage 字段解析 TokenUsage
pub fn parse_usage(val: &Value) -> TokenUsage {
    let input_tokens = val
        .get("input_tokens")
        .or_else(|| val.get("prompt_tokens"))
        .and_then(Value::as_u64)
        .unwrap_or(0) as u32;

    let output_tokens = val
        .get("output_tokens")
        .or_else(|| val.get("completion_tokens"))
        .and_then(Value::as_u64)
        .unwrap_or(0) as u32;

    let cache_hit = val
        .get("prompt_cache_hit_tokens")
        .and_then(Value::as_u64)
        .or_else(|| {
            val.get("prompt_tokens_details")
                .and_then(|d| d.get("cached_tokens"))
                .and_then(Value::as_u64)
        })
        .unwrap_or(0) as u32;

    let cache_miss = val
        .get("prompt_cache_miss_tokens")
        .and_then(Value::as_u64)
        .or_else(|| {
            let miss: u64 = input_tokens.saturating_sub(cache_hit) as u64;
            (miss > 0).then_some(miss)
        })
        .unwrap_or(input_tokens as u64) as u32;

    TokenUsage::new(input_tokens, output_tokens, cache_hit, cache_miss)
}

/// 将 请求体写入调试日志，只保留最近 100 条
fn log_api_request(dir: &std::path::Path, body: &Value) {
    let path = dir.join("api_debug.json");

    // 读取现有记录
    let mut entries: Vec<Value> = if path.exists() {
        std::fs::read_to_string(&path)
            .ok()
            .and_then(|s| serde_json::from_str(&s).ok())
            .unwrap_or_default()
    } else {
        // 确保目录存在
        let _ = std::fs::create_dir_all(dir);
        Vec::new()
    };

    // 在开头插入新条目
    entries.insert(0, body.clone());

    // 只保留 100 条
    entries.truncate(100);

    // 写回文件
    if let Ok(json) = serde_json::to_string_pretty(&entries) {
        let _ = std::fs::write(&path, json);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn test_parse_usage_v4() {
        let val = json!({"input_tokens":1500,"output_tokens":300,"prompt_cache_hit_tokens":1200,"prompt_cache_miss_tokens":300});
        let u = parse_usage(&val);
        assert_eq!(u.input_tokens, 1500);
        assert_eq!(u.cache_hit_tokens, 1200);
    }

    #[test]
    fn test_parse_usage_v4_old_names() {
        let val = json!({"prompt_tokens":500,"completion_tokens":100,"prompt_cache_hit_tokens":400,"prompt_cache_miss_tokens":100});
        let u = parse_usage(&val);
        assert_eq!(u.input_tokens, 500);
        assert_eq!(u.cache_hit_tokens, 400);
    }

    #[test]
    fn test_parse_usage_v3() {
        let val = json!({"prompt_tokens":1500,"completion_tokens":300,"prompt_tokens_details":{"cached_tokens":1000}});
        let u = parse_usage(&val);
        assert_eq!(u.input_tokens, 1500);
        assert_eq!(u.cache_hit_tokens, 1000);
    }

    #[test]
    fn test_parse_usage_no_cache() {
        let val = json!({"input_tokens":500,"output_tokens":100});
        let u = parse_usage(&val);
        assert_eq!(u.cache_hit_tokens, 0);
        assert_eq!(u.cache_miss_tokens, 500);
    }
}
