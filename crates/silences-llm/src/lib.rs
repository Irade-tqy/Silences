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

use anyhow::{Result, anyhow};
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
            .map_err(|e| anyhow::anyhow!("预热请求失败: {e:#}"))?;

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

        Ok(info)
    }

    pub fn with_tokenizer(mut self, path: &str) -> Self {
        self.tokenizer = Tokenizer::from_file(path).ok();
        if self.tokenizer.is_none() {
            eprintln!("[llm] 警告: 无法加载 tokenizer {path}");
        }
        self
    }

    /// 构建 API messages JSON（含工具调用）
    ///
    /// 自动清理孤立的 tool_calls 和 tool_result 双向防御：
    /// - 孤立 tool_call：assistant 的 tool_calls 在后续没有对应 tool_result → 跳过或裁剪
    /// - 孤立 tool_result：tool result 之前没有 assistant 声明过该 tool_call_id → 跳过
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
        // 第二遍：跟踪已声明的 tool_call_id（assistant 消息的 tool_calls 中的 id）
        let mut declared_ids: HashSet<String> = HashSet::new();
        if let Some(sys) = system {
            api.push(json!({"role": "system", "content": sys}));
        }
        for msg in messages {
            // 先收集此消息中声明的 tool_call_id（在跳过检查之前，确保后续 tool result 能匹配）
            if let Some(ref tc) = msg.tool_calls {
                for tc_item in tc {
                    declared_ids.insert(tc_item.id.clone());
                }
            }

            // 检查此消息的 tool_calls 是否有孤立的（无对应 tool_result）
            let all_tc_orphaned = msg.tool_calls.as_ref().map_or(false, |tc| {
                tc.iter().all(|tc| !completed_ids.contains(tc.id.as_str()))
            });

            // 如果 assistant 消息的所有 tool_calls 都是孤立的且 content 为空，
            // 直接跳过这条消息 —— 留它在消息列表中只会让 LLM 困惑
            if msg.role == "assistant" && all_tc_orphaned && msg.content.is_empty() {
                continue;
            }

            // 跳过孤儿 tool result：没有 preceding assistant 声明过这个 tool_call_id
            if msg.role == "tool" {
                if let Some(ref tcid) = msg.tool_call_id {
                    if !declared_ids.contains(tcid.as_str()) {
                        continue;
                    }
                }
            }

            let content_val = if msg.content.is_empty() && msg.tool_calls.is_some() && !all_tc_orphaned {
                Value::Null
            } else {
                json!(msg.content)
            };
            let mut m = json!({"role": msg.role, "content": content_val});
            // DeepSeek thinking 模式：所有 assistant 消息必须携带 reasoning_content
            if msg.role == "assistant" {
                m["reasoning_content"] = json!(msg.reasoning_content.as_deref().unwrap_or(""));
            }
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
            .map_err(|e| anyhow::anyhow!("发送聊天请求失败: {e:#}"))?;

        let status = response.status();
        if !status.is_success() {
            let text = response.text().await.unwrap_or_default();
            anyhow::bail!("DeepSeek API error HTTP {status}: {text}");
        }

        let byte_stream = Box::pin(response.bytes_stream());

        // 获取递增序号用于捕获文件命名
        let call_seq = {
            let seq_path = self.debug_dir.as_ref().map(|d| d.join("_call_seq.txt"));
            if let Some(ref p) = seq_path {
                let prev = std::fs::read_to_string(p).ok().and_then(|s| s.trim().parse::<u32>().ok()).unwrap_or(0);
                let next = prev + 1;
                let _ = std::fs::write(p, next.to_string());
                next
            } else {
                0
            }
        };

        Ok(ChatStream {
            byte_stream,
            line_buf: String::new(),
            usage: None,
            ended: false,
            request_body: body,
            capture_dir: self.debug_dir.clone(),
            captured_deltas: Vec::new(),
            call_seq,
        })
    }
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
    // 响应捕获（配对请求体 + 响应事件 + usage）
    request_body: Value,
    capture_dir: Option<PathBuf>,
    captured_deltas: Vec<Value>,
    call_seq: u32,
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
                        self.usage = Some(parse_usage(usage_val));
                    }
                }

                if let Some(choices) = val.get("choices").and_then(Value::as_array) {
                    for choice in choices {
                        if let Some(delta) = choice.get("delta") {
                if let Some(r) = delta.get("reasoning_content").and_then(Value::as_str) {
                                if !r.is_empty() {
                                    let delta = StreamDelta::Reasoning(r.to_string());
                                    self.record_delta(&delta);
                                    return Ok(Some(delta));
                                }
                            }
                            if let Some(c) = delta.get("content").and_then(Value::as_str) {
                                if !c.is_empty() {
                                    let delta = StreamDelta::Text(c.to_string());
                                    self.record_delta(&delta);
                                    return Ok(Some(delta));
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
                                    let delta = StreamDelta::ToolCall { index, id, name, arguments };
                                    self.record_delta(&delta);
                                    return Ok(Some(delta));
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
        let usage = self.usage.take();

        // 流结束：将配对请求+响应写入捕获文件
        if let Some(ref dir) = self.capture_dir {
            // 收集完整的响应事件（去重，仅记录非 reasoning 结论）
            let mut text_parts: Vec<String> = Vec::new();
            let mut tool_calls: Vec<Value> = Vec::new();
            for entry in &self.captured_deltas {
                match entry["type"].as_str() {
                    Some("text") => text_parts.push(entry["content"].as_str().unwrap_or("").to_string()),
                    Some("tool_call") => {
                        let idx = tool_calls.iter().position(|t| t["index"] == entry["index"]);
                        if let Some(i) = idx {
                            // 追加 arguments
                            if let Some(args) = entry["function"]["arguments"].as_str() {
                                if let Some(existing) = tool_calls[i]["function"]["arguments"].as_str() {
                                    let merged = format!("{existing}{args}");
                                    tool_calls[i]["function"]["arguments"] = json!(merged);
                                }
                            }
                            if let Some(id) = entry["id"].as_str() {
                                if tool_calls[i]["id"].is_null() {
                                    tool_calls[i]["id"] = json!(id);
                                }
                            }
                            if let Some(name) = entry["function"]["name"].as_str() {
                                if tool_calls[i]["function"]["name"].is_null() {
                                    tool_calls[i]["function"]["name"] = json!(name);
                                }
                            }
                        } else {
                            tool_calls.push(entry.clone());
                        }
                    }
                    _ => {}
                }
            }

            let record = json!({
                "call_seq": self.call_seq,
                "request": self.request_body,
                "response": {
                    "text": text_parts.join(""),
                    "tool_calls": tool_calls,
                    "usage": usage,
                },
                "captured_deltas": self.captured_deltas,
            });

            // 保存到配对日志文件
            let pair_path = dir.join("api_pairs.jsonl");
            if let Ok(line) = serde_json::to_string(&record) {
                use std::io::Write;
                let _ = std::fs::OpenOptions::new()
                    .create(true)
                    .append(true)
                    .open(&pair_path)
                    .map(|mut f| {
                        let _ = writeln!(f, "{line}");
                    });
            }
        }

        usage
    }

    /// 记录响应 delta 到 captured_deltas
    fn record_delta(&mut self, delta: &StreamDelta) {
        let entry = match delta {
            StreamDelta::Text(t) => json!({"type": "text", "content": t}),
            StreamDelta::Reasoning(r) => json!({"type": "reasoning", "content": r}),
            StreamDelta::ToolCall { index, id, name, arguments } => json!({
                "type": "tool_call",
                "index": index,
                "id": id,
                "function": {
                    "name": name,
                    "arguments": arguments,
                }
            }),
        };
        self.captured_deltas.push(entry);
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
    use silences_core::{ToolCallFunction, ToolCallValue};

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

    // ── build_api_messages: reasoning_content ──────────────────────────

    #[test]
    fn test_build_api_messages_assistant_with_reasoning() {
        let mut msg = Message::new("assistant", "你好，我是助手。");
        msg.reasoning_content = Some("让我想想……".into());
        let result = LlmClient::build_api_messages(&[msg], None);
        assert_eq!(result.len(), 1);
        assert_eq!(result[0]["reasoning_content"], "让我想想……");
    }

    #[test]
    fn test_build_api_messages_assistant_without_reasoning() {
        // assistant 消息没有 reasoning_content → 必须补 ""
        let msg = Message::new("assistant", "你好。");
        let result = LlmClient::build_api_messages(&[msg], None);
        assert_eq!(result.len(), 1);
        assert_eq!(result[0]["reasoning_content"], "");
    }

    #[test]
    fn test_build_api_messages_user_no_reasoning() {
        // user 消息不能有 reasoning_content
        let msg = Message::new("user", "用户提问");
        let result = LlmClient::build_api_messages(&[msg], None);
        assert_eq!(result.len(), 1);
        assert!(result[0].get("reasoning_content").is_none());
    }

    #[test]
    fn test_build_api_messages_tool_no_reasoning() {
        // tool 消息不能有 reasoning_content
        let mut msg = Message::new("tool", "{\"result\": \"ok\"}");
        msg.tool_call_id = Some("call_123".into());
        // 需要一个 assistant 先声明 tool_call_id，否则 tool 会被当作孤儿跳过
        let asst_with_declare = Message::new_tool_call(vec![
            ToolCallValue {
                id: "call_123".into(),
                call_type: "function".into(),
                function: ToolCallFunction {
                    name: "get_weather".into(),
                    arguments: "{}".into(),
                },
            },
        ]);
        let result = LlmClient::build_api_messages(&[asst_with_declare, msg], None);
        assert_eq!(result.len(), 2);
        assert!(result[1].get("reasoning_content").is_none());
        // assistant 还是要有 reasoning_content
        assert_eq!(result[0]["reasoning_content"], "");
    }

    #[test]
    fn test_build_api_messages_assistant_with_tool_calls_no_reasoning() {
        // assistant 有 tool_calls + 文本内容但无 reasoning → reasoning_content 必须补 ""
        let mut asst = Message::new("assistant", "我来搜索一下。");
        asst.tool_calls = Some(vec![
            ToolCallValue {
                id: "call_456".into(),
                call_type: "function".into(),
                function: ToolCallFunction {
                    name: "search".into(),
                    arguments: "{\"q\":\"test\"}".into(),
                },
            },
        ]);
        let result = LlmClient::build_api_messages(&[asst], None);
        assert_eq!(result.len(), 1);
        assert_eq!(result[0]["reasoning_content"], "");
    }

    #[test]
    fn test_build_api_messages_system_no_reasoning() {
        // system 消息不应有 reasoning_content
        let result = LlmClient::build_api_messages(&[], Some("你是助手。"));
        assert_eq!(result.len(), 1);
        assert_eq!(result[0]["role"], "system");
        assert!(result[0].get("reasoning_content").is_none());
    }

    #[test]
    fn test_build_api_messages_mixed_roles() {
        // 混合场景：user → assistant(有思考) → tool → assistant(无思考) → user
        let msgs = vec![
            Message::new("user", "今天天气如何？"),
            {
                let mut m = Message::new("assistant", "让我查一下天气。");
                m.reasoning_content = Some("分析用户意图……".into());
                m
            },
            {
                let mut m = Message::new("user", "谢谢。");
                m.name = Some("user".into());
                m
            },
        ];
        let result = LlmClient::build_api_messages(&msgs, Some("你是一个天气助手。"));
        // system + 3 messages
        assert_eq!(result.len(), 4);
        assert_eq!(result[0]["role"], "system");
        assert!(result[0].get("reasoning_content").is_none());
        assert_eq!(result[1]["role"], "user");
        assert!(result[1].get("reasoning_content").is_none());
        assert_eq!(result[2]["role"], "assistant");
        assert_eq!(result[2]["reasoning_content"], "分析用户意图……");
        assert_eq!(result[3]["role"], "user");
        assert!(result[3].get("reasoning_content").is_none());
    }

    #[test]
    fn test_build_api_messages_orphan_assistant_skipped() {
        // 孤立 tool_call 且 content 为空 → 整条 assistant 跳过，不应产生 reasoning_content 问题
        let asst = Message::new_tool_call(vec![
            ToolCallValue {
                id: "call_orphan".into(),
                call_type: "function".into(),
                function: ToolCallFunction {
                    name: "nonexistent".into(),
                    arguments: "{}".into(),
                },
            },
        ]);
        let result = LlmClient::build_api_messages(&[asst], None);
        // 没有对应的 tool_result → 整条跳过
        assert_eq!(result.len(), 0);
    }

    #[test]
    fn test_build_api_messages_orphan_tool_result_skipped() {
        // 孤立的 tool result（无对应 assistant 声明）→ 跳过
        let mut tool_msg = Message::new("tool", "结果");
        tool_msg.tool_call_id = Some("call_nonexistent".into());
        let result = LlmClient::build_api_messages(&[tool_msg], None);
        assert_eq!(result.len(), 0);
    }
}
