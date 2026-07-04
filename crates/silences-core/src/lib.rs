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

/// 任务项
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TaskItem {
    pub id: String,
    pub description: String,
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
    /// 上下文回退（兼作消息边界）
    #[serde(rename = "context_rollback")]
    ContextRollback,
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
}

fn default_warmup() -> bool { true }

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
}

/// 当前会话运行时状态（后端计算，前端只负责渲染）
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SessionState {
    /// 最后一次 LLM 调用时发送的 messages 快照
    pub context: Vec<Message>,
    /// 当前任务队列中的待办任务
    pub tasks: Vec<TaskItem>,
    /// agent 运行状态："idle" | "running" | "paused"
    #[serde(default = "default_status")]
    pub status: String,
}

fn default_status() -> String { "idle".to_string() }
