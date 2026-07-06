//! Silences 库模式接口
//!
//! 提供阻塞式 API，供 AgentBench 等外部 Rust crate 调用。
//!
//! # 使用示例
//!
//! ```no_run
//! use silences_lib::{Silences, SilencesConfig};
//! use std::path::PathBuf;
//!
//! # async fn example() -> anyhow::Result<()> {
//! let silences = Silences::new(SilencesConfig {
//!     db_path: "./silences.db".into(),
//!     api_key: "sk-xxx".into(),
//!     base_url: None,
//!     model: None,
//!     system_prompt: None,
//!     project_root: Some(PathBuf::from(".")),
//!     tool_limits: None,
//!     warmup_enabled: false,
//! })?;
//!
//! let session_id = silences.create_session().await?;
//! let result = silences.process_turn(&session_id, "Hello").await?;
//! println!("助理回复: {}", result.reply);
//! # Ok(())
//! # }
//! ```

use std::collections::{HashMap, HashSet};
use std::path::PathBuf;
use std::sync::{Arc, Mutex as StdMutex};

use silences_agent::agent::{run_agent_blocking, prepare_agent_context};
use silences_agent::toolcall::{self, ReadTracker};
use silences_agent::toolcall::regret::ToolHistory;
use silences_agent::queue::TaskQueue;
use silences_core::{Message, TokenUsage, ToolLimits, ToolCallValue, ToolCallFunction};
use silences_db::Db;
use silences_llm::LlmClient;
use tokio::sync::Mutex;

/// Silences 库模式配置
#[derive(Debug, Clone)]
pub struct SilencesConfig {
    /// SQLite 数据库路径
    pub db_path: String,
    /// DeepSeek API Key
    pub api_key: String,
    /// DeepSeek API 地址（None = 默认）
    pub base_url: Option<String>,
    /// DeepSeek 模型名（None = 默认）
    pub model: Option<String>,
    /// 可选的 system prompt
    pub system_prompt: Option<String>,
    /// 项目根目录（用于 SILENCES.md / CONTEXT.md）
    pub project_root: Option<PathBuf>,
    /// 工具截断限制（None = 默认值）
    pub tool_limits: Option<ToolLimits>,
    /// 是否启用 prefix cache 预热
    pub warmup_enabled: bool,
}

/// 一轮 process_turn 的结果
#[derive(Debug, Clone)]
pub struct TurnResult {
    /// assistant 文本回复
    pub reply: String,
    /// 完整上下文消息（处理后）
    pub messages: Vec<Message>,
    /// Token 用量
    pub usage: Option<TokenUsage>,
}

/// Silences 库模式主结构体
///
/// 持有 LLM 客户端、数据库、任务队列等共享状态。
/// 方法都是线程安全的（内部用 Arc/Mutex 保护）。
pub struct Silences {
    llm: LlmClient,
    db: Arc<Mutex<Db>>,
    /// 每个会话的 agent 工具历史（用于 regret，跨 process_turn 调用保持）
    tool_histories: StdMutex<HashMap<String, Arc<Mutex<ToolHistory>>>>,
    task_queue: Arc<TaskQueue>,
    agent_contexts: Arc<Mutex<HashMap<String, Vec<Message>>>>,
    project_root: Option<PathBuf>,
    system_prompt: Option<String>,
    warmup_enabled: bool,
    tool_limits: ToolLimits,
}

impl Silences {
    /// 创建新的 Silences 实例
    ///
    /// 打开数据库、初始化 LLM 客户端。
    pub fn new(config: SilencesConfig) -> anyhow::Result<Self> {
        let db = Db::open(&config.db_path)?;

        let base_url = config.base_url.clone().unwrap_or_else(|| "https://api.deepseek.com".to_string());
        let model = config.model.clone().unwrap_or_else(|| "deepseek-v4-flash".to_string());
        let llm = LlmClient::new(config.api_key.clone(), base_url, model);

        // warmup_enabled 默认 true（与 server 一致），但允许 caller 显式关闭
        let warmup_enabled = config.warmup_enabled;

        Ok(Self {
            llm,
            db: Arc::new(Mutex::new(db)),
            tool_histories: StdMutex::new(HashMap::new()),
            task_queue: Arc::new(TaskQueue::new()),
            agent_contexts: Arc::new(Mutex::new(HashMap::new())),
            project_root: config.project_root,
            system_prompt: config.system_prompt,
            warmup_enabled,
            tool_limits: config.tool_limits.unwrap_or_default(),
        })
    }

    /// 发送消息，等待 agent 完成，返回回复
    ///
    /// 如果 `session_id` 对应的会话不存在，会自动创建。
    pub async fn process_turn(&self, session_id: &str, message: &str) -> anyhow::Result<TurnResult> {
        // 检查 API key（与 server 一致）
        if self.llm.api_key_snapshot().map_or(true, |k| k.is_empty()) {
            anyhow::bail!("未配置 API Key");
        }

        // 1. 准备上下文
        let mut prep = prepare_agent_context(
            &self.db,
            self.project_root.as_deref(),
            Some(session_id.to_string()),
            message,
            self.system_prompt.as_deref(),
        )
        .await?;

        // 2. 获取或创建此会话的工具历史（跨 process_turn 调用保持，与 server 一致）
        let tool_history = {
            let mut histories = self.tool_histories.lock().unwrap();
            histories
                .entry(session_id.to_string())
                .or_insert_with(|| Arc::new(Mutex::new(ToolHistory::new(5))))
                .clone()
        };
        let read_tracker: ReadTracker = Arc::new(Mutex::new(HashSet::new()));
        let tools = toolcall::all_tools(
            tool_history.clone(),
            read_tracker,
            self.task_queue.clone(),
            Some(prep.session_dir.clone()),
            self.tool_limits,
        );

        // ── 自动任务包装：无活跃任务时，把用户消息自动包装为 task ──
        if !self.task_queue.has_active() {
            let msg_preview: String = message.chars().take(10).collect();
            let task_id = format!("处理用户消息：{}", msg_preview);
            let description = message;
            let add_tc_id = "call_add".to_string();
            let start_tc_id = "call_start".to_string();

            // 合成 assistant tool_call 消息（add_task + start_task 并行）
            let asst_msg = Message::new_tool_call(vec![
                ToolCallValue {
                    id: add_tc_id.clone(),
                    call_type: "function".into(),
                    function: ToolCallFunction {
                        name: "add_task".into(),
                        arguments: serde_json::json!({"id": task_id, "description": description}).to_string(),
                    },
                },
                ToolCallValue {
                    id: start_tc_id.clone(),
                    call_type: "function".into(),
                    function: ToolCallFunction {
                        name: "start_task".into(),
                        arguments: serde_json::json!({"task_id": task_id, "description": description}).to_string(),
                    },
                },
            ]);
            prep.messages.push(asst_msg);

            // 执行 add_task
            if let Ok(outcome) = toolcall::execute_tool(
                &tools, "add_task",
                serde_json::json!({"id": task_id, "description": description}),
            ).await {
                prep.messages.push(Message::new_tool_result(&add_tc_id, &outcome.summary));
            }

            // 执行 start_task
            if let Ok(outcome) = toolcall::execute_tool(
                &tools, "start_task",
                serde_json::json!({"task_id": task_id, "description": description}),
            ).await {
                prep.messages.push(Message::new_tool_result(&start_tc_id, &outcome.summary));
            }

            eprintln!("[silences-lib] 自动包装为任务: {task_id}");
        }

        // 3. 阻塞式运行 agent
        let output = run_agent_blocking(
            self.llm.clone_for_agent(),
            tools,
            prep.messages,
            self.system_prompt.clone(),
            tool_history,
            self.db.clone(),
            prep.session_id,
            Some(prep.session_dir),
            0, // tool_delay_ms — lib 模式不延迟
            self.warmup_enabled,
            self.task_queue.clone(),
            self.agent_contexts.clone(),
        )
        .await?;

        Ok(TurnResult {
            reply: output.assistant_reply,
            messages: output.messages,
            usage: output.total_usage,
        })
    }

    /// 获取会话的所有非 hidden 上下文消息
    ///
    /// 返回的消息已过滤掉隐藏的（rollback 标记）消息。
    pub async fn get_context(&self, session_id: &str) -> anyhow::Result<Vec<Message>> {
        let db = self.db.lock().await;
        db.get_messages(session_id)
    }

    /// 创建新会话，返回会话 ID
    pub async fn create_session(&self) -> anyhow::Result<String> {
        let db = self.db.lock().await;
        db.create_session()
    }

    /// 从已有上下文列表创建会话（预填充消息）
    ///
    /// 创建新会话并将 `context` 中的所有消息写入该会话。
    /// 返回新会话的 ID。
    pub async fn create_session_from_context(&self, context: Vec<Message>) -> anyhow::Result<String> {
        let db = self.db.lock().await;
        let sid = db.create_session()?;
        for msg in context {
            db.save_message(&sid, &msg)?;
        }
        Ok(sid)
    }

    /// 清理会话（删除消息、用量、工具历史、上下文快照、文件系统上下文）
    pub async fn delete_session(&self, session_id: &str) -> anyhow::Result<()> {
        let db = self.db.lock().await;
        db.delete_session(session_id)?;
        // 清理工具历史（与 server 一致）
        if let Ok(mut histories) = self.tool_histories.lock() {
            histories.remove(session_id);
        }
        // 清理上下文快照（与 server 一致）
        {
            let mut ctx = self.agent_contexts.lock().await;
            ctx.remove(session_id);
        }
        // 清理文件系统上下文
        if let Some(ref root) = self.project_root {
            silences_agent::context::delete_session_context(root, session_id);
        }
        Ok(())
    }
}
