//! Silences 核心类型：Message, Session, Cost

use serde::{Deserialize, Serialize};

/// 单条对话消息
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct Message {
    pub role: String,       // "system" | "user" | "assistant" | "tool"
    pub content: String,
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

/// SSE 事件类型（server → CLI）
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
    /// 工具调用
    #[serde(rename = "tool_calling")]
    ToolCalling { name: String, args: String },
    /// 工具结果
    #[serde(rename = "tool_result")]
    ToolResult { name: String, summary: String },
    /// 错误
    #[serde(rename = "error")]
    Error { message: String },
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
}

/// 更新设置的请求体
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SettingsUpdate {
    pub api_key: Option<String>,
    pub system_prompt: Option<String>,
}
