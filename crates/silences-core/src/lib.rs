//! Silences 核心类型：Message, Session, Cost

use serde::{Deserialize, Serialize};
use std::sync::atomic::{AtomicBool, Ordering};

/// 单条对话消息
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct Message {
    pub role: String,       // "system" | "user" | "assistant" | "tool"
    pub content: String,
    /// 消息发送者的名称标记，用于区分用户和系统注入指令
    /// "user" = 用户原始输入, "orch" = system orchestrator 指令
    #[serde(skip_serializing_if = "Option::is_none", default)]
    pub name: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none", default)]
    pub reasoning_content: Option<String>,  // DeepSeek v4 thinking 模式
    /// assistant 消息的工具调用（DeepSeek / OpenAI 格式）
    #[serde(skip_serializing_if = "Option::is_none", default)]
    pub tool_calls: Option<Vec<ToolCallValue>>,
    /// tool 角色消息关联的 tool_call_id
    #[serde(skip_serializing_if = "Option::is_none", default)]
    pub tool_call_id: Option<String>,
}

impl Message {
    /// 快速构造普通消息
    pub fn new(role: &str, content: &str) -> Self {
        Self {
            role: role.to_string(),
            content: content.to_string(),
            name: None,
            reasoning_content: None,
            tool_calls: None,
            tool_call_id: None,
        }
    }

    /// 构造带 name 的用户消息
    pub fn new_user(name: &str, content: &str) -> Self {
        Self {
            role: "user".into(),
            content: content.to_string(),
            name: Some(name.to_string()),
            reasoning_content: None,
            tool_calls: None,
            tool_call_id: None,
        }
    }

    /// 快速构造 assistant tool_call 消息
    pub fn new_tool_call(tool_calls: Vec<ToolCallValue>) -> Self {
        Self {
            role: "assistant".into(),
            content: String::new(),
            name: None,
            reasoning_content: None,
            tool_calls: Some(tool_calls),
            tool_call_id: None,
        }
    }

    /// 快速构造 tool 结果消息
    pub fn new_tool_result(tool_call_id: &str, content: &str) -> Self {
        Self {
            role: "tool".into(),
            content: content.to_string(),
            name: None,
            reasoning_content: None,
            tool_calls: None,
            tool_call_id: Some(tool_call_id.to_string()),
        }
    }
}

/// Tool call（DeepSeek / OpenAI 格式）
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ToolCallValue {
    pub id: String,
    #[serde(rename = "type")]
    pub call_type: String,
    pub function: ToolCallFunction,
}

/// Tool call 的 function 部分
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ToolCallFunction {
    pub name: String,
    /// JSON 字符串格式的参数
    pub arguments: String,
}

/// 一次会话
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Session {
    pub id: String,
    pub created_at: String,  // ISO 8601
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub preview: Option<String>,  // 第一条用户消息预览
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,  // 用户自定义名称
}

/// Token 用量与花费
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TokenUsage {
    pub input_tokens: u32,
    pub output_tokens: u32,
    pub cache_hit_tokens: u32,
    pub cache_miss_tokens: u32,
    pub cost_yuan: f64,
}

impl TokenUsage {
    pub fn new(
        input_tokens: u32,
        output_tokens: u32,
        cache_hit_tokens: u32,
        cache_miss_tokens: u32,
    ) -> Self {
        let cost_yuan = compute_cost(input_tokens, output_tokens, cache_hit_tokens, cache_miss_tokens);
        Self { input_tokens, output_tokens, cache_hit_tokens, cache_miss_tokens, cost_yuan }
    }
}

/// 定价常量（元/百万 token）
const PRICE_CACHE_HIT: f64 = 0.02;
const PRICE_CACHE_MISS: f64 = 1.0;
const PRICE_OUTPUT: f64 = 2.0;

/// 计算 API 花费（人民币）
pub fn compute_cost(_input: u32, output: u32, cache_hit: u32, cache_miss: u32) -> f64 {
    let hit_cost = cache_hit as f64 * PRICE_CACHE_HIT / 1_000_000.0;
    let miss_cost = cache_miss as f64 * PRICE_CACHE_MISS / 1_000_000.0;
    let out_cost = output as f64 * PRICE_OUTPUT / 1_000_000.0;
    hit_cost + miss_cost + out_cost
}

/// 前端渲染用消息（tool_results 已嵌入，无 tool 角色消息）
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ViewMessage {
    pub role: String,
    pub content: String,
    #[serde(skip_serializing_if = "Option::is_none", default)]
    pub reasoning_content: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none", default)]
    pub tool_calls: Option<Vec<ViewToolCall>>,
}

/// 前端渲染用工具调用（结果已嵌入）
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ViewToolCall {
    pub name: String,
    pub args: String,
    #[serde(skip_serializing_if = "Option::is_none", default)]
    pub result: Option<String>,
}

/// 将原始消息列表转换为前端渲染格式：
/// 1. 过滤 tool 角色消息
/// 2. 将 tool_result 嵌入到对应 assistant 消息的 tool_calls 中
pub fn messages_to_view(msgs: Vec<Message>) -> Vec<ViewMessage> {
    // 收集 tool_call_id → content 映射（克隆字符串避免借用冲突）
    let mut results: std::collections::HashMap<String, String> = std::collections::HashMap::new();
    for msg in &msgs {
        if msg.role == "tool" {
            if let Some(ref id) = msg.tool_call_id {
                results.insert(id.clone(), msg.content.clone());
            }
        }
    }

    msgs.into_iter()
        .filter(|m| m.role != "tool")
        .map(|m| {
            let tool_calls = m.tool_calls.map(|tc| {
                tc.into_iter().map(|tc| ViewToolCall {
                    name: tc.function.name,
                    args: tc.function.arguments,
                    result: results.get(&tc.id).cloned(),
                }).collect()
            });
            ViewMessage {
                role: m.role,
                content: m.content,
                reasoning_content: m.reasoning_content,
                tool_calls,
            }
        })
        .collect()
}

/// Agent 运行标志：停止 + 暂停（线程安全，通过 Arc 共享）
#[derive(Debug)]
pub struct RunFlags {
    stop: AtomicBool,
    pause: AtomicBool,
}

impl RunFlags {
    pub fn new() -> Self {
        Self {
            stop: AtomicBool::new(false),
            pause: AtomicBool::new(false),
        }
    }

    pub fn signal_stop(&self)  { self.stop.store(true, Ordering::Relaxed); }
    pub fn should_stop(&self) -> bool { self.stop.load(Ordering::Relaxed) }

    pub fn signal_pause(&self)  { self.pause.store(true, Ordering::Relaxed); }
    pub fn signal_resume(&self) { self.pause.store(false, Ordering::Relaxed); }
    pub fn should_pause(&self) -> bool { self.pause.load(Ordering::Relaxed) }
}

/// 工具截断限制配置（各工具共用的硬限制，通过 all_tools 传入）
#[derive(Debug, Clone, Copy, Serialize, Deserialize)]
pub struct ToolLimits {
    /// command stdout 最大 token 数（超出截断，完整内容存 console 文件）
    pub command_stdout_max_tok: usize,
    /// command stderr 最大 token 数
    pub command_stderr_max_tok: usize,
    /// glance 读取文件头部注释的最大行数（超出给出提示）
    pub glance_max_comment_lines: usize,
    /// grep 摘要最多显示多少条匹配（超出后截断，完整内容存 console 文件）
    pub grep_max_shown_matches: usize,
    /// grep 匹配行上下各显示多少行作为上下文
    pub grep_context_lines: usize,
    /// glance 目录模式下最多显示多少条目（超出后截断，完整内容存 console 文件）
    pub glance_max_shown_items: usize,
    /// find 最多显示多少匹配（超出后截断，完整内容存 console 文件）
    pub find_max_shown_items: usize,
    /// edit/block_edit/replace 匹配失败时显示行号上下文的行数
    pub edit_context_lines: usize,
}

impl Default for ToolLimits {
    fn default() -> Self {
        Self {
            command_stdout_max_tok: 2000,
            command_stderr_max_tok: 1000,
            glance_max_comment_lines: 20,
            grep_max_shown_matches: 8,
            grep_context_lines: 2,
            glance_max_shown_items: 50,
            find_max_shown_items: 50,
            edit_context_lines: 5,
        }
    }
}

/// 设置 agent 状态的请求
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SetStateRequest {
    pub action: String, // "pause" | "resume" | "stop"
}

/// SSE 事件类型（server → 前端）
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type")]
pub enum SseEvent {
    /// 文本块
    #[serde(rename = "text")]
    Text { content: String },
    /// 思考过程
    #[serde(rename = "reasoning")]
    Reasoning { content: String },
    /// 会话 ID（新建会话时返回）
    #[serde(rename = "session")]
    Session { id: String },
    /// Token 用量
    #[serde(rename = "usage")]
    Usage(TokenUsage),
    /// 工具调用（result 为 None 表示执行中，Some 表示已完成）
    #[serde(rename = "tool_call")]
    ToolCall { id: String, name: String, args: String, result: Option<String> },
    /// 消息边界：前端应关闭当前流式消息并开启新消息
    #[serde(rename = "message_boundary")]
    MessageBoundary,
    /// agent 已暂停
    #[serde(rename = "paused")]
    Paused,
    /// agent 已恢复运行
    #[serde(rename = "resumed")]
    Resumed,
    /// 错误
    #[serde(rename = "error")]
    Error { message: String },
}

/// 手术刀 Agent 请求
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SurgeryRequest {
    pub prompt: String,
}

/// 聊天响应（SSE 流结束后 server 也返回 JSON，包含 session_id 和 usage）
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ChatResponse {
    pub session_id: String,
    pub usage: TokenUsage,
}

/// 聊天请求（CLI → server）
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ChatRequest {
    pub session_id: Option<String>,  // None = 新建会话
    pub message: String,
    pub system: Option<String>,      // 可选 system prompt
    #[serde(default = "default_true")]
    pub stream: bool,                // false = 非流式
}

fn default_true() -> bool { true }

/// 设置（API key / system prompt）
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Settings {
    pub api_key: Option<String>,
    pub system_prompt: Option<String>,
    /// 每轮 tool loop 延迟（毫秒），用于调试慢速观察
    pub tool_delay_ms: u64,
    /// 是否启用 agent loop 提示词预热（prefix cache 激活）
    #[serde(default = "default_warmup")]
    pub warmup_enabled: bool,
    /// 发送消息时自动清理上下文（去 reasoning/过滤失败 tool call/精简结果）
    #[serde(default = "default_auto_collapse")]
    pub auto_collapse_prev: bool,
}

fn default_warmup() -> bool { true }
fn default_auto_collapse() -> bool { true }

/// 更新设置的请求体
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SettingsUpdate {
    pub api_key: Option<String>,
    pub system_prompt: Option<String>,
    /// 每轮 tool loop 延迟（毫秒），传递 0 或 None 表示不延迟
    #[serde(default)]
    pub tool_delay_ms: Option<u64>,
    /// 是否启用 prefix cache 预热，None 表示不更新
    #[serde(default)]
    pub warmup_enabled: Option<bool>,
    /// 发送消息时自动清理上下文，None 表示不更新
    #[serde(default)]
    pub auto_collapse_prev: Option<bool>,
}

/// 当前会话运行时状态（后端计算，前端只负责渲染）
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SessionState {
    /// 最后一次 LLM 调用时发送的 messages 快照
    pub context: Vec<Message>,
    /// agent 运行状态："idle" | "running" | "paused"
    #[serde(default = "default_status")]
    pub status: String,
}

fn default_status() -> String { "idle".to_string() }

#[cfg(test)]
mod tests {
    use super::*;

    // ─── Message ────────────────────────────────────────────────────────

    #[test]
    fn message_new_sets_role_and_content() {
        let m = Message::new("user", "hello");
        assert_eq!(m.role, "user");
        assert_eq!(m.content, "hello");
        assert!(m.name.is_none());
        assert!(m.reasoning_content.is_none());
        assert!(m.tool_calls.is_none());
        assert!(m.tool_call_id.is_none());
    }

    #[test]
    fn message_new_empty_strings() {
        let m = Message::new("", "");
        assert_eq!(m.role, "");
        assert_eq!(m.content, "");
    }

    #[test]
    fn message_new_long_content() {
        let long = "a".repeat(100_000);
        let m = Message::new("user", &long);
        assert_eq!(m.content.len(), 100_000);
    }

    #[test]
    fn message_new_user_sets_role_user_and_name() {
        let m = Message::new_user("Alice", "hi");
        assert_eq!(m.role, "user");
        assert_eq!(m.content, "hi");
        assert_eq!(m.name, Some("Alice".into()));
    }

    #[test]
    fn message_new_user_name_none() {
        let m = Message::new_user("", "content");
        assert_eq!(m.name, Some("".into()));
    }

    #[test]
    fn message_new_tool_call_sets_role_assistant_and_tool_calls() {
        let tc = ToolCallValue {
            id: "call_1".into(),
            call_type: "function".into(),
            function: ToolCallFunction {
                name: "get_weather".into(),
                arguments: r#"{"city":"Beijing"}"#.into(),
            },
        };
        let m = Message::new_tool_call(vec![tc]);
        assert_eq!(m.role, "assistant");
        assert_eq!(m.content, "");
        assert!(m.name.is_none());
        let calls = m.tool_calls.unwrap();
        assert_eq!(calls.len(), 1);
        assert_eq!(calls[0].id, "call_1");
    }

    #[test]
    fn message_new_tool_call_multiple() {
        let tcs = (0..5)
            .map(|i| ToolCallValue {
                id: format!("call_{}", i),
                call_type: "function".into(),
                function: ToolCallFunction {
                    name: "fn".into(),
                    arguments: "{}".into(),
                },
            })
            .collect();
        let m = Message::new_tool_call(tcs);
        assert_eq!(m.tool_calls.unwrap().len(), 5);
    }

    #[test]
    fn message_new_tool_call_empty_vec() {
        let m = Message::new_tool_call(vec![]);
        assert_eq!(m.role, "assistant");
        assert_eq!(m.tool_calls.unwrap().len(), 0);
    }

    #[test]
    fn message_new_tool_result_sets_role_tool_and_tool_call_id() {
        let m = Message::new_tool_result("call_xyz", "result data");
        assert_eq!(m.role, "tool");
        assert_eq!(m.content, "result data");
        assert_eq!(m.tool_call_id, Some("call_xyz".into()));
        assert!(m.tool_calls.is_none());
    }

    #[test]
    fn message_serialize_roundtrip() {
        let m = Message::new("assistant", "hello world");
        let json = serde_json::to_string(&m).unwrap();
        let deserialized: Message = serde_json::from_str(&json).unwrap();
        assert_eq!(deserialized.role, "assistant");
        assert_eq!(deserialized.content, "hello world");
    }

    #[test]
    fn message_serialize_omits_optional_none() {
        let m = Message::new("user", "hi");
        let json = serde_json::to_string(&m).unwrap();
        assert!(!json.contains("name"));
        assert!(!json.contains("tool_calls"));
    }

    #[test]
    fn message_serialize_includes_optional_some() {
        let m = Message::new_user("Bob", "hello");
        let json = serde_json::to_string(&m).unwrap();
        assert!(json.contains(r#""name":"Bob""#));
    }

    #[test]
    fn message_default_is_empty() {
        let m = Message::default();
        assert_eq!(m.role, "");
        assert_eq!(m.content, "");
        assert!(m.name.is_none());
    }

    // ─── ToolCallValue ───────────────────────────────────────────────────

    #[test]
    fn tool_call_value_construction() {
        let tc = ToolCallValue {
            id: "call_1".into(),
            call_type: "function".into(),
            function: ToolCallFunction {
                name: "search".into(),
                arguments: "{\"q\":\"test\"}".into(),
            },
        };
        assert_eq!(tc.id, "call_1");
        assert_eq!(tc.call_type, "function");
        assert_eq!(tc.function.name, "search");
    }

    #[test]
    fn tool_call_value_type_renamed_in_json() {
        let tc = ToolCallValue {
            id: "c1".into(),
            call_type: "function".into(),
            function: ToolCallFunction {
                name: "f".into(),
                arguments: "{}".into(),
            },
        };
        let json = serde_json::to_string(&tc).unwrap();
        // "type" (not "call_type") should appear in JSON
        assert!(json.contains(r#""type":"function""#));
    }

    #[test]
    fn tool_call_value_roundtrip() {
        let tc = ToolCallValue {
            id: "c1".into(),
            call_type: "function".into(),
            function: ToolCallFunction {
                name: "f".into(),
                arguments: r#"{"x":1}"#.into(),
            },
        };
        let json = serde_json::to_string(&tc).unwrap();
        let back: ToolCallValue = serde_json::from_str(&json).unwrap();
        assert_eq!(back.id, "c1");
        assert_eq!(back.call_type, "function");
        assert_eq!(back.function.name, "f");
    }

    // ─── ToolCallFunction ────────────────────────────────────────────────

    #[test]
    fn tool_call_function_roundtrip() {
        let f = ToolCallFunction {
            name: "get_weather".into(),
            arguments: r#"{"city":"Beijing"}"#.into(),
        };
        let json = serde_json::to_string(&f).unwrap();
        let back: ToolCallFunction = serde_json::from_str(&json).unwrap();
        assert_eq!(back.name, "get_weather");
        assert_eq!(back.arguments, r#"{"city":"Beijing"}"#);
    }

    #[test]
    fn tool_call_function_empty_arguments() {
        let f = ToolCallFunction {
            name: "noop".into(),
            arguments: "".into(),
        };
        let json = serde_json::to_string(&f).unwrap();
        let back: ToolCallFunction = serde_json::from_str(&json).unwrap();
        assert_eq!(back.arguments, "");
    }

    // ─── Session ─────────────────────────────────────────────────────────

    #[test]
    fn session_construction() {
        let s = Session {
            id: "sess_1".into(),
            created_at: "2025-01-01T00:00:00Z".into(),
            preview: Some("first msg".into()),
            name: Some("my chat".into()),
        };
        assert_eq!(s.id, "sess_1");
        assert_eq!(s.name, Some("my chat".into()));
        assert_eq!(s.preview, Some("first msg".into()));
    }

    #[test]
    fn session_optional_fields_default_to_none() {
        let json = r#"{"id":"s1","created_at":"2025-01-01T00:00:00Z"}"#;
        let s: Session = serde_json::from_str(json).unwrap();
        assert_eq!(s.id, "s1");
        assert!(s.preview.is_none());
        assert!(s.name.is_none());
    }

    #[test]
    fn session_optional_fields_omitted_when_none() {
        let s = Session {
            id: "s1".into(),
            created_at: "now".into(),
            preview: None,
            name: None,
        };
        let json = serde_json::to_string(&s).unwrap();
        assert!(!json.contains("preview"));
        assert!(!json.contains("name"));
    }

    #[test]
    fn session_roundtrip() {
        let s = Session {
            id: "s1".into(),
            created_at: "now".into(),
            preview: Some("hello".into()),
            name: None,
        };
        let json = serde_json::to_string(&s).unwrap();
        let back: Session = serde_json::from_str(&json).unwrap();
        assert_eq!(back.id, "s1");
        assert_eq!(back.preview, Some("hello".into()));
        assert!(back.name.is_none());
    }

    // ─── TokenUsage ──────────────────────────────────────────────────────

    #[test]
    fn token_usage_new_computes_cost() {
        // cache_hit=1_000_000 → 0.02 yuan; cache_miss=0 → 0; output=0 → 0
        let tu = TokenUsage::new(0, 0, 1_000_000, 0);
        assert_eq!(tu.cost_yuan, 0.02);
    }

    #[test]
    fn token_usage_cost_cache_miss() {
        let tu = TokenUsage::new(0, 0, 0, 1_000_000);
        assert_eq!(tu.cost_yuan, 1.0);
    }

    #[test]
    fn token_usage_cost_output() {
        let tu = TokenUsage::new(0, 1_000_000, 0, 0);
        assert_eq!(tu.cost_yuan, 2.0);
    }

    #[test]
    fn token_usage_cost_all_combined() {
        let tu = TokenUsage::new(0, 500_000, 1_000_000, 500_000);
        // 0.02*1 + 1.0*0.5 + 2.0*0.5 = 0.02 + 0.5 + 1.0 = 1.52
        assert!((tu.cost_yuan - 1.52).abs() < 1e-9);
    }

    #[test]
    fn token_usage_cost_zero() {
        let tu = TokenUsage::new(0, 0, 0, 0);
        assert_eq!(tu.cost_yuan, 0.0);
    }

    #[test]
    fn token_usage_cost_large_values() {
        let tu = TokenUsage::new(0, 10_000_000, 5_000_000, 10_000_000);
        // output: 10_000_000 * 2.0 / 1_000_000 = 20.0
        // cache_hit: 5_000_000 * 0.02 / 1_000_000 = 0.1
        // cache_miss: 10_000_000 * 1.0 / 1_000_000 = 10.0
        // total = 30.1
        assert!((tu.cost_yuan - 30.1).abs() < 1e-9);
    }

    #[test]
    fn token_usage_fields() {
        let tu = TokenUsage::new(100, 200, 30, 40);
        assert_eq!(tu.input_tokens, 100);
        assert_eq!(tu.output_tokens, 200);
        assert_eq!(tu.cache_hit_tokens, 30);
        assert_eq!(tu.cache_miss_tokens, 40);
    }

    #[test]
    fn token_usage_roundtrip() {
        let tu = TokenUsage::new(10, 20, 5, 3);
        let json = serde_json::to_string(&tu).unwrap();
        let back: TokenUsage = serde_json::from_str(&json).unwrap();
        assert_eq!(back.input_tokens, 10);
        assert_eq!(back.output_tokens, 20);
        assert!((back.cost_yuan - tu.cost_yuan).abs() < 1e-12);
    }

    // ─── compute_cost ────────────────────────────────────────────────────

    #[test]
    fn compute_cost_zero_inputs() {
        assert_eq!(compute_cost(0, 0, 0, 0), 0.0);
    }

    #[test]
    fn compute_cost_cache_hit_only() {
        assert_eq!(compute_cost(0, 0, 1_000_000, 0), 0.02);
    }

    #[test]
    fn compute_cost_cache_miss_only() {
        assert_eq!(compute_cost(0, 0, 0, 1_000_000), 1.0);
    }

    #[test]
    fn compute_cost_output_only() {
        assert_eq!(compute_cost(0, 1_000_000, 0, 0), 2.0);
    }

    #[test]
    fn compute_cost_mixed() {
        // 1M cache_hit = 0.02, 0.5M cache_miss = 0.5, 0.25M output = 0.5 → 1.02
        let cost = compute_cost(0, 250_000, 1_000_000, 500_000);
        assert!((cost - 1.02).abs() < 1e-9);
    }

    #[test]
    fn compute_cost_large_values() {
        // 100M output = 200 yuan
        let cost = compute_cost(0, 100_000_000, 0, 0);
        assert_eq!(cost, 200.0);
    }

    #[test]
    fn compute_cost_fractional_tokens() {
        // sub-million values produce fractional results
        let cost = compute_cost(0, 0, 500_000, 0);
        assert_eq!(cost, 0.01); // 500k * 0.02 / 1M = 0.01
    }

    #[test]
    fn compute_cost_input_is_discarded() {
        // The _input parameter is explicitly ignored
        let cost = compute_cost(999_999_999, 1_000_000, 0, 0);
        assert_eq!(cost, 2.0); // only output counts
    }

    // ─── ViewMessage ─────────────────────────────────────────────────────

    #[test]
    fn view_message_construction() {
        let vm = ViewMessage {
            role: "assistant".into(),
            content: "hi".into(),
            reasoning_content: Some("thinking...".into()),
            tool_calls: None,
        };
        assert_eq!(vm.role, "assistant");
        assert_eq!(vm.content, "hi");
        assert_eq!(vm.reasoning_content, Some("thinking...".into()));
    }

    #[test]
    fn view_message_optional_fields_omitted_when_none() {
        let vm = ViewMessage {
            role: "user".into(),
            content: "hello".into(),
            reasoning_content: None,
            tool_calls: None,
        };
        let json = serde_json::to_string(&vm).unwrap();
        assert!(!json.contains("reasoning_content"));
        assert!(!json.contains("tool_calls"));
    }

    // ─── ViewToolCall ────────────────────────────────────────────────────

    #[test]
    fn view_tool_call_construction() {
        let vtc = ViewToolCall {
            name: "search".into(),
            args: r#"{"q":"x"}"#.into(),
            result: Some("found".into()),
        };
        assert_eq!(vtc.name, "search");
        assert_eq!(vtc.result, Some("found".into()));
    }

    #[test]
    fn view_tool_call_result_none() {
        let vtc = ViewToolCall {
            name: "f".into(),
            args: "{}".into(),
            result: None,
        };
        let json = serde_json::to_string(&vtc).unwrap();
        assert!(!json.contains("result"));
    }

    #[test]
    fn view_tool_call_result_some_included() {
        let vtc = ViewToolCall {
            name: "f".into(),
            args: "{}".into(),
            result: Some("ok".into()),
        };
        let json = serde_json::to_string(&vtc).unwrap();
        assert!(json.contains(r#""result":"ok""#));
    }

    // ─── messages_to_view ────────────────────────────────────────────────

    #[test]
    fn messages_to_view_empty() {
        let result = messages_to_view(vec![]);
        assert!(result.is_empty());
    }

    #[test]
    fn messages_to_view_filters_tool_role() {
        let msgs = vec![
            Message::new("user", "hi"),
            Message::new("tool", "result"),
            Message::new("assistant", "hello"),
        ];
        let result = messages_to_view(msgs);
        assert_eq!(result.len(), 2);
        assert!(result.iter().all(|vm| vm.role != "tool"));
    }

    #[test]
    fn messages_to_view_embeds_tool_result() {
        let fn_ = ToolCallFunction {
            name: "get_weather".into(),
            arguments: r#"{"city":"Beijing"}"#.into(),
        };
        let tc = ToolCallValue {
            id: "call_1".into(),
            call_type: "function".into(),
            function: fn_,
        };
        let msgs = vec![
            Message::new_user("user", "weather?"),
            Message::new_tool_call(vec![tc]),
            Message::new_tool_result("call_1", "25 C"),
        ];
        let result = messages_to_view(msgs);
        assert_eq!(result.len(), 2); // user + assistant (tool filtered)
        let vm = &result[1];
        assert_eq!(vm.role, "assistant");
        let tc = vm.tool_calls.as_ref().unwrap();
        assert_eq!(tc.len(), 1);
        assert_eq!(tc[0].result, Some("25 C".into()));
    }

    #[test]
    fn messages_to_view_multiple_tool_calls_with_results() {
        let tcs: Vec<_> = (0..3)
            .map(|i| ToolCallValue {
                id: format!("call_{}", i),
                call_type: "function".into(),
                function: ToolCallFunction {
                    name: format!("fn_{}", i),
                    arguments: "{}".into(),
                },
            })
            .collect();
        let mut msgs = vec![Message::new_tool_call(tcs)];
        for i in 0..3 {
            msgs.push(Message::new_tool_result(&format!("call_{}", i), &format!("res_{}", i)));
        }
        let result = messages_to_view(msgs);
        assert_eq!(result.len(), 1);
        let vm = &result[0];
        let calls = vm.tool_calls.as_ref().unwrap();
        assert_eq!(calls.len(), 3);
        for (i, call) in calls.iter().enumerate() {
            assert_eq!(call.result, Some(format!("res_{}", i)));
            assert_eq!(call.name, format!("fn_{}", i));
        }
    }

    #[test]
    fn messages_to_view_missing_tool_call_id() {
        let tc = ToolCallValue {
            id: "call_1".into(),
            call_type: "function".into(),
            function: ToolCallFunction {
                name: "f".into(),
                arguments: "{}".into(),
            },
        };
        let msgs = vec![
            Message::new_tool_call(vec![tc]),
            // tool result with a different id => no match
            Message::new_tool_result("call_missing", "data"),
        ];
        let result = messages_to_view(msgs);
        assert_eq!(result.len(), 1);
        let calls = result[0].tool_calls.as_ref().unwrap();
        assert_eq!(calls.len(), 1);
        assert!(calls[0].result.is_none());
    }

    #[test]
    fn messages_to_view_only_tool_messages_yields_empty() {
        let msgs = vec![
            Message::new_tool_result("c1", "r1"),
            Message::new_tool_result("c2", "r2"),
        ];
        let result = messages_to_view(msgs);
        assert!(result.is_empty());
    }

    // ─── RunFlags ────────────────────────────────────────────────────────

    #[test]
    fn run_flags_new_starts_false() {
        let rf = RunFlags::new();
        assert!(!rf.should_stop());
        assert!(!rf.should_pause());
    }

    #[test]
    fn run_flags_stop_lifecycle() {
        let rf = RunFlags::new();
        assert!(!rf.should_stop());
        rf.signal_stop();
        assert!(rf.should_stop());
    }

    #[test]
    fn run_flags_pause_resume_lifecycle() {
        let rf = RunFlags::new();
        assert!(!rf.should_pause());
        rf.signal_pause();
        assert!(rf.should_pause());
        rf.signal_resume();
        assert!(!rf.should_pause());
    }

    #[test]
    fn run_flags_stop_and_pause_independent() {
        let rf = RunFlags::new();
        rf.signal_stop();
        rf.signal_pause();
        assert!(rf.should_stop());
        assert!(rf.should_pause());
        rf.signal_resume();
        assert!(rf.should_stop()); // resume does not affect stop
        assert!(!rf.should_pause());
    }

    #[test]
    fn run_flags_send_sync() {
        // Compile-time check: RunFlags must be Send + Sync (uses AtomicBool)
        fn assert_send_sync<T: Send + Sync>() {}
        assert_send_sync::<RunFlags>();
    }

    // ─── ToolLimits ──────────────────────────────────────────────────────

    #[test]
    fn tool_limits_default_values() {
        let tl = ToolLimits::default();
        assert_eq!(tl.command_stdout_max_tok, 2000);
        assert_eq!(tl.command_stderr_max_tok, 1000);
        assert_eq!(tl.glance_max_comment_lines, 20);
        assert_eq!(tl.grep_max_shown_matches, 8);
        assert_eq!(tl.grep_context_lines, 2);
        assert_eq!(tl.glance_max_shown_items, 50);
        assert_eq!(tl.find_max_shown_items, 50);
        assert_eq!(tl.edit_context_lines, 5);
    }

    #[test]
    fn tool_limits_roundtrip() {
        let tl = ToolLimits::default();
        let json = serde_json::to_string(&tl).unwrap();
        let back: ToolLimits = serde_json::from_str(&json).unwrap();
        assert_eq!(back.command_stdout_max_tok, 2000);
    }

    #[test]
    fn tool_limits_custom_values() {
        let tl = ToolLimits {
            command_stdout_max_tok: 100,
            command_stderr_max_tok: 50,
            glance_max_comment_lines: 5,
            grep_max_shown_matches: 1,
            grep_context_lines: 0,
            glance_max_shown_items: 10,
            find_max_shown_items: 20,
            edit_context_lines: 3,
        };
        let json = serde_json::to_string(&tl).unwrap();
        let back: ToolLimits = serde_json::from_str(&json).unwrap();
        assert_eq!(back.command_stdout_max_tok, 100);
        assert_eq!(back.grep_context_lines, 0);
    }

    // ─── SetStateRequest ─────────────────────────────────────────────────

    #[test]
    fn set_state_request_roundtrip() {
        let req = SetStateRequest { action: "pause".into() };
        let json = serde_json::to_string(&req).unwrap();
        let back: SetStateRequest = serde_json::from_str(&json).unwrap();
        assert_eq!(back.action, "pause");
    }

    #[test]
    fn set_state_request_deserialize_stop() {
        let back: SetStateRequest = serde_json::from_str(r#"{"action":"stop"}"#).unwrap();
        assert_eq!(back.action, "stop");
    }

    // ─── SseEvent ────────────────────────────────────────────────────────

    #[test]
    fn sse_event_text_serialize() {
        let e = SseEvent::Text { content: "hello".into() };
        let json = serde_json::to_string(&e).unwrap();
        assert!(json.contains(r#""type":"text""#));
        assert!(json.contains(r#""content":"hello""#));
        let back: SseEvent = serde_json::from_str(&json).unwrap();
        match back {
            SseEvent::Text { content } => assert_eq!(content, "hello"),
            _ => panic!("expected Text"),
        }
    }

    #[test]
    fn sse_event_reasoning_serialize() {
        let e = SseEvent::Reasoning { content: "think".into() };
        let json = serde_json::to_string(&e).unwrap();
        assert!(json.contains(r#""type":"reasoning""#));
        let back: SseEvent = serde_json::from_str(&json).unwrap();
        match back {
            SseEvent::Reasoning { content } => assert_eq!(content, "think"),
            _ => panic!("expected Reasoning"),
        }
    }

    #[test]
    fn sse_event_session_serialize() {
        let e = SseEvent::Session { id: "s1".into() };
        let json = serde_json::to_string(&e).unwrap();
        assert!(json.contains(r#""type":"session""#));
        let back: SseEvent = serde_json::from_str(&json).unwrap();
        match back {
            SseEvent::Session { id } => assert_eq!(id, "s1"),
            _ => panic!("expected Session"),
        }
    }

    #[test]
    fn sse_event_usage_serialize() {
        let usage = TokenUsage::new(10, 20, 5, 3);
        let e = SseEvent::Usage(usage);
        let json = serde_json::to_string(&e).unwrap();
        assert!(json.contains(r#""type":"usage""#));
        assert!(json.contains(r#""input_tokens":10"#));
        let back: SseEvent = serde_json::from_str(&json).unwrap();
        match back {
            SseEvent::Usage(u) => assert_eq!(u.output_tokens, 20),
            _ => panic!("expected Usage"),
        }
    }

    #[test]
    fn sse_event_tool_call_serialize() {
        let e = SseEvent::ToolCall {
            id: "c1".into(),
            name: "search".into(),
            args: r#"{"q":"x"}"#.into(),
            result: Some("found".into()),
        };
        let json = serde_json::to_string(&e).unwrap();
        assert!(json.contains(r#""type":"tool_call""#));
        assert!(json.contains(r#""result":"found""#));
        let back: SseEvent = serde_json::from_str(&json).unwrap();
        match back {
            SseEvent::ToolCall { id, name, result, .. } => {
                assert_eq!(id, "c1");
                assert_eq!(name, "search");
                assert_eq!(result, Some("found".into()));
            }
            _ => panic!("expected ToolCall"),
        }
    }

    #[test]
    fn sse_event_tool_call_result_none() {
        let e = SseEvent::ToolCall {
            id: "c1".into(),
            name: "f".into(),
            args: "{}".into(),
            result: None,
        };
        let json = serde_json::to_string(&e).unwrap();
        // result is serialized as null when None (no skip_serializing_if)
        assert!(json.contains(r#""result":null"#));
    }

    #[test]
    fn sse_event_message_boundary() {
        let e = SseEvent::MessageBoundary;
        let json = serde_json::to_string(&e).unwrap();
        assert_eq!(json, r#"{"type":"message_boundary"}"#);
        let back: SseEvent = serde_json::from_str(&json).unwrap();
        assert!(matches!(back, SseEvent::MessageBoundary));
    }

    #[test]
    fn sse_event_paused() {
        let e = SseEvent::Paused;
        let json = serde_json::to_string(&e).unwrap();
        assert_eq!(json, r#"{"type":"paused"}"#);
    }

    #[test]
    fn sse_event_resumed() {
        let e = SseEvent::Resumed;
        let json = serde_json::to_string(&e).unwrap();
        assert_eq!(json, r#"{"type":"resumed"}"#);
    }

    #[test]
    fn sse_event_error() {
        let e = SseEvent::Error { message: "boom".into() };
        let json = serde_json::to_string(&e).unwrap();
        assert!(json.contains(r#""type":"error""#));
        assert!(json.contains(r#""message":"boom""#));
        let back: SseEvent = serde_json::from_str(&json).unwrap();
        match back {
            SseEvent::Error { message } => assert_eq!(message, "boom"),
            _ => panic!("expected Error"),
        }
    }

    #[test]
    fn sse_event_all_variants_have_type_tag() {
        let variants: Vec<SseEvent> = vec![
            SseEvent::Text { content: "".into() },
            SseEvent::Reasoning { content: "".into() },
            SseEvent::Session { id: "".into() },
            SseEvent::Usage(TokenUsage::new(0, 0, 0, 0)),
            SseEvent::ToolCall { id: "".into(), name: "".into(), args: "".into(), result: None },
            SseEvent::MessageBoundary,
            SseEvent::Paused,
            SseEvent::Resumed,
            SseEvent::Error { message: "".into() },
        ];
        for v in &variants {
            let json = serde_json::to_string(v).unwrap();
            assert!(json.starts_with(r#"{"type":""#), "variant {:?} missing type tag", v);
        }
    }

    // ─── ChatResponse ────────────────────────────────────────────────────

    #[test]
    fn chat_response_construction() {
        let usage = TokenUsage::new(10, 20, 5, 3);
        let cr = ChatResponse {
            session_id: "s1".into(),
            usage: usage.clone(),
        };
        assert_eq!(cr.session_id, "s1");
        assert_eq!(cr.usage.output_tokens, 20);
    }

    #[test]
    fn chat_response_roundtrip() {
        let cr = ChatResponse {
            session_id: "s1".into(),
            usage: TokenUsage::new(1, 2, 0, 0),
        };
        let json = serde_json::to_string(&cr).unwrap();
        let back: ChatResponse = serde_json::from_str(&json).unwrap();
        assert_eq!(back.session_id, "s1");
        assert_eq!(back.usage.output_tokens, 2);
    }

    // ─── ChatRequest ─────────────────────────────────────────────────────

    #[test]
    fn chat_request_construction() {
        let cr = ChatRequest {
            session_id: Some("s1".into()),
            message: "hello".into(),
            system: Some("You are a helpful assistant.".into()),
            stream: false,
        };
        assert_eq!(cr.session_id, Some("s1".into()));
        assert_eq!(cr.message, "hello");
        assert_eq!(cr.system, Some("You are a helpful assistant.".into()));
        assert!(!cr.stream);
    }

    #[test]
    fn chat_request_stream_defaults_to_true() {
        let json = r#"{"message":"hello"}"#;
        let cr: ChatRequest = serde_json::from_str(json).unwrap();
        assert!(cr.stream);
    }

    #[test]
    fn chat_request_session_id_and_system_optional() {
        let json = r#"{"message":"hi"}"#;
        let cr: ChatRequest = serde_json::from_str(json).unwrap();
        assert!(cr.session_id.is_none());
        assert!(cr.system.is_none());
    }

    #[test]
    fn chat_request_roundtrip() {
        let cr = ChatRequest {
            session_id: None,
            message: "hello".into(),
            system: None,
            stream: true,
        };
        let json = serde_json::to_string(&cr).unwrap();
        let back: ChatRequest = serde_json::from_str(&json).unwrap();
        assert_eq!(back.message, "hello");
        assert!(back.stream);
    }

    // ─── Settings ────────────────────────────────────────────────────────

    #[test]
    fn settings_warmup_enabled_default_true() {
        let json = r#"{"tool_delay_ms":100}"#;
        let s: Settings = serde_json::from_str(json).unwrap();
        assert!(s.warmup_enabled);
    }

    #[test]
    fn settings_construction() {
        let s = Settings {
            api_key: Some("sk-xxx".into()),
            system_prompt: Some("You are helpful.".into()),
            tool_delay_ms: 500,
            warmup_enabled: false,
        };
        assert_eq!(s.api_key, Some("sk-xxx".into()));
        assert_eq!(s.tool_delay_ms, 500);
        assert!(!s.warmup_enabled);
    }

    #[test]
    fn settings_roundtrip() {
        let s = Settings {
            api_key: None,
            system_prompt: None,
            tool_delay_ms: 0,
            warmup_enabled: true,
        };
        let json = serde_json::to_string(&s).unwrap();
        let back: Settings = serde_json::from_str(&json).unwrap();
        assert!(back.warmup_enabled);
        assert_eq!(back.tool_delay_ms, 0);
    }

    // ─── SettingsUpdate ──────────────────────────────────────────────────

    #[test]
    fn settings_update_deserialize_partial() {
        let json = r#"{"api_key":"sk-new"}"#;
        let su: SettingsUpdate = serde_json::from_str(json).unwrap();
        assert_eq!(su.api_key, Some("sk-new".into()));
        assert!(su.system_prompt.is_none());
        assert!(su.tool_delay_ms.is_none());
        assert!(su.warmup_enabled.is_none());
    }

    #[test]
    fn settings_update_all_fields() {
        let json = r#"{"api_key":"k","system_prompt":"p","tool_delay_ms":200,"warmup_enabled":false}"#;
        let su: SettingsUpdate = serde_json::from_str(json).unwrap();
        assert_eq!(su.api_key, Some("k".into()));
        assert_eq!(su.system_prompt, Some("p".into()));
        assert_eq!(su.tool_delay_ms, Some(200));
        assert_eq!(su.warmup_enabled, Some(false));
    }

    #[test]
    fn settings_update_empty_json() {
        let json = r#"{}"#;
        let su: SettingsUpdate = serde_json::from_str(json).unwrap();
        assert!(su.api_key.is_none());
        assert!(su.system_prompt.is_none());
        assert!(su.tool_delay_ms.is_none());
        assert!(su.warmup_enabled.is_none());
    }

    #[test]
    fn settings_update_roundtrip() {
        let su = SettingsUpdate {
            api_key: Some("k".into()),
            system_prompt: None,
            tool_delay_ms: Some(100),
            warmup_enabled: None,
        };
        let json = serde_json::to_string(&su).unwrap();
        let back: SettingsUpdate = serde_json::from_str(&json).unwrap();
        assert_eq!(back.api_key, Some("k".into()));
        assert!(back.system_prompt.is_none());
        assert_eq!(back.tool_delay_ms, Some(100));
    }

    // ─── SessionState ────────────────────────────────────────────────────

    #[test]
    fn session_state_status_defaults_to_idle() {
        let json = r#"{"context":[]}"#;
        let ss: SessionState = serde_json::from_str(json).unwrap();
        assert_eq!(ss.status, "idle");
    }

    #[test]
    fn session_state_construction() {
        let ss = SessionState {
            context: vec![Message::new("user", "hello")],
            status: "running".into(),
        };
        assert_eq!(ss.context.len(), 1);
        assert_eq!(ss.status, "running");
    }

    #[test]
    fn session_state_roundtrip() {
        let ss = SessionState {
            context: vec![],
            status: "paused".into(),
        };
        let json = serde_json::to_string(&ss).unwrap();
        let back: SessionState = serde_json::from_str(&json).unwrap();
        assert_eq!(back.status, "paused");
    }

    #[test]
    fn session_state_empty_roundtrip() {
        let ss = SessionState {
            context: vec![],
            status: "idle".into(),
        };
        let json = serde_json::to_string(&ss).unwrap();
        let back: SessionState = serde_json::from_str(&json).unwrap();
        assert!(back.context.is_empty());
    }
}
