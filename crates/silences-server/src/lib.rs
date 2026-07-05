//! Silences 后端服务
//!
//! 提供 `POST /chat` 端点，接收用户消息，启动 agent 循环，
//! 以 SSE 流式返回文本回复 + tool call 摘要 + token 用量 + 会话 ID。

use std::collections::{HashMap, HashSet};
use std::path::PathBuf;
use std::pin::Pin;
use std::sync::atomic::{AtomicBool, AtomicU64};
use std::sync::{Arc, Mutex as StdMutex};
use std::task::{Context, Poll};

use axum::{
    Json, Router,
    extract::{Path, State},
    http::StatusCode,
    response::sse::{Event, Sse},
    routing::{delete, get, post, put},
};
use futures_util::stream::Stream;
use silences_agent::agent::{run_agent, AgentEvent};
use silences_agent::queue::TaskQueue;
use silences_agent::toolcall::regret::ToolHistory;
use silences_agent::toolcall::{self, ReadTracker, ToolDef};
use silences_core::{ChatRequest, Message, RunFlags, Session, SessionState, SetStateRequest, Settings, SettingsUpdate, SseEvent, ToolCallFunction, ToolCallValue, ViewMessage, messages_to_view};
use silences_db::Db;
use silences_llm::LlmClient;
use tokio::sync::Mutex;
use tokio_stream::wrappers::ReceiverStream;
use tower_http::cors::CorsLayer;

/// 会话重命名请求
#[derive(serde::Deserialize)]
struct RenameRequest {
    name: String,
}

/// 应用状态
struct AppState {
    llm: LlmClient,
    db: Arc<Mutex<Db>>,
    /// 每个会话的 agent 工具历史（用于 regret）
    agent_histories: Mutex<HashMap<String, Arc<Mutex<ToolHistory>>>>,
    /// 当前正在运行的 agent 运行标志（stop / pause）
    active_runs: Arc<Mutex<HashMap<String, Arc<RunFlags>>>>,
    /// 当前设置的 system prompt（运行时可变）
    system_prompt: StdMutex<Option<String>>,
    /// 项目根目录（用于读取 SILENCES.md / CONTEXT.md）
    project_root: Option<PathBuf>,
    /// 工具循环延迟（毫秒），每个 round 之间暂停
    tool_delay_ms: AtomicU64,
    /// 是否启用 agent loop prefix cache 预热
    warmup_enabled: AtomicBool,
    /// 动态任务优先队列
    task_queue: Arc<TaskQueue>,
    /// 每个会话最后一次发给 LLM 的 messages 快照
    agent_contexts: Arc<Mutex<HashMap<String, Vec<Message>>>>,
}

/// 流包装器：在 client 断开时不从 active_runs 中移除停止标志
///（使得刷新页面后 stop 按钮仍能工作）
struct CleanupStream<S> {
    inner: S,
    session_id: String,
}

impl<S> Drop for CleanupStream<S> {
    fn drop(&mut self) {
        // 不清理 active_runs —— SSE 断开不代表 agent 应该停止
        // agent 自然退出时由 run_agent 自行清理，手动停止由 handle_stop_agent 清理
        eprintln!("[CleanupStream] session={} SSE 连接已断开，agent 继续在后台运行",
            &self.session_id[..8.min(self.session_id.len())]);
    }
}

impl<S: Stream<Item = T> + Unpin, T> Stream for CleanupStream<S> {
    type Item = T;
    fn poll_next(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Option<Self::Item>> {
        let this = self.get_mut();
        Pin::new(&mut this.inner).poll_next(cx)
    }
}

/// 启动服务
pub async fn serve(
    llm: LlmClient,
    db: Db,
    bind: &str,
    project_root: Option<PathBuf>,
) -> anyhow::Result<()> {
    // 从 DB 加载已保存的 system prompt
    let saved_system = db.get_setting("system_prompt").ok().flatten()
        .filter(|s| !s.is_empty());
    if let Some(ref s) = saved_system {
        eprintln!("[serve] 从 DB 加载 system prompt ({} 字符)", s.len());
    } else {
        eprintln!("[serve] 未保存 system prompt");
    }

    // 保底使用 cwd（不靠 .git 目录瞎找）
    let project_root = project_root.or_else(|| std::env::current_dir().ok());
    let tool_delay_ms = db.get_setting("tool_delay_ms").ok().flatten()
        .and_then(|s| s.parse().ok())
        .unwrap_or(0);
    let warmup_enabled = db.get_setting("warmup_enabled").ok().flatten()
        .and_then(|s| s.parse::<u8>().ok())
        .map(|v| v != 0)
        .unwrap_or(true);

    let state = Arc::new(AppState {
        llm,
        db: Arc::new(Mutex::new(db)),
        agent_histories: Mutex::new(HashMap::new()),
        active_runs: Arc::new(Mutex::new(HashMap::new())),
        system_prompt: StdMutex::new(saved_system),
        project_root,
        tool_delay_ms: AtomicU64::new(tool_delay_ms),
        warmup_enabled: AtomicBool::new(warmup_enabled),
        task_queue: Arc::new(TaskQueue::new()),
        agent_contexts: Arc::new(Mutex::new(HashMap::new())),
    });

    let app = Router::new()
        .route("/chat", post(handle_chat))
        .route("/sessions", get(handle_list_sessions))
        .route("/sessions/{id}/messages", get(handle_session_messages))
        .route("/sessions/{id}/usage", get(handle_session_usage))
        .route("/sessions/{id}/state", get(handle_session_state))
        .route("/sessions/{id}/rename", put(handle_rename_session))
        .route("/sessions/{id}", delete(handle_delete_session))
        .route("/sessions/{id}/set_state", post(handle_set_state))
        .route("/settings", get(handle_get_settings))
        .route("/settings", put(handle_put_settings))
        .layer(CorsLayer::permissive())
        .with_state(state);

    let listener = tokio::net::TcpListener::bind(bind).await?;
    eprintln!("[silences-server] 启动于 {bind}");
    axum::serve(listener, app).await?;
    Ok(())
}

/// 处理聊天请求
async fn handle_chat(
    State(state): State<Arc<AppState>>,
    Json(req): Json<ChatRequest>,
) -> Result<Sse<impl Stream<Item = Result<Event, axum::Error>>>, (StatusCode, String)> {
    // 检查 API key
    if state.llm.api_key_snapshot().map_or(true, |k| k.is_empty()) {
        return Err((StatusCode::BAD_REQUEST,
            "请先在设置页面中配置 API Key".to_string()));
    }

    // 如果请求未提供 system prompt，使用当前设置中的
    let system = req.system.clone().or_else(|| {
        state.system_prompt.lock().ok().and_then(|sp| sp.clone())
    });

    // 获取或创建会话
    let is_new_session = req.session_id.as_ref().map_or(true, |s| s.is_empty());
    let session_id = if !is_new_session {
        req.session_id.clone().unwrap()
    } else {
        let db = state.db.lock().await;
        let sid = db.create_session().map_err(|e| {
            (StatusCode::INTERNAL_SERVER_ERROR, format!("DB error: {e}"))
        })?;
        // 新会话初始化上下文文件
        if let Some(ref root) = state.project_root {
            let _ = silences_agent::context::init_session_context(root, &sid);
        }
        sid
    };

    // 保存用户消息
    {
        let db = state.db.lock().await;
        let msg = Message::new_user("user", &req.message);
        db.save_message(&session_id, &msg).map_err(|e| {
            (StatusCode::INTERNAL_SERVER_ERROR, format!("DB error: {e}"))
        })?;
    }

    // 加载历史消息
    let history = {
        let db = state.db.lock().await;
        let msgs = db.get_messages(&session_id).map_err(|e| {
            (StatusCode::INTERNAL_SERVER_ERROR, format!("DB error: {e}"))
        })?;
        msgs
    };

    // 读取 SILENCES.md（会话级），CONTEXT.md 由 agent 延迟注入
    let ctx = silences_agent::context::load_project_context(
        state.project_root.as_deref(),
        Some(&session_id),
    );

    // 构造 context：SILENCES.md 在最前，然后历史消息
    let silences_name = ctx.session_dir.join("SILENCES.md").to_string_lossy().to_string();
    let mut context: Vec<Message> = Vec::new();
    if let Some(ref silences) = ctx.silences_md {
        context.push(Message::new_user(&silences_name, silences));
    }
    context.extend(history);

    // 日志：本次请求的完整上下文
    eprintln!("——[REQ]——————————————————————————————");
    eprintln!("  session={} msgs={} new={} silences={} ctx_delta={}",
        &session_id[..8.min(session_id.len())],
        context.len(),
        is_new_session,
        ctx.silences_md.is_some(),
        ctx.context_delta.is_some(),
    );
    for (i, msg) in context.iter().enumerate() {
        let preview: String = msg.content.chars().take(120).collect();
        let rc = if msg.reasoning_content.is_some() { " +reasoning" } else { "" };
        let tc = if msg.tool_calls.is_some() { " +tool_calls" } else { "" };
        let name_tag = msg.name.as_ref().map(|n| format!(" @{n}")).unwrap_or_default();
        if msg.content.len() > 120 {
            eprintln!("  [{i}][{}]{name_tag}{rc}{tc} {}...", msg.role, preview);
        } else {
            eprintln!("  [{i}][{}]{name_tag}{rc}{tc} {}", msg.role, preview);
        }
    }
    if let Some(sys) = &system {
        let clipped: String = sys.chars().take(120).collect();
        eprintln!("  [system] {clipped}...");
    }

    // 获取或创建此会话的工具历史
    let tool_history = {
        let mut histories = state.agent_histories.lock().await;
        histories
            .entry(session_id.clone())
            .or_insert_with(|| Arc::new(Mutex::new(ToolHistory::new(5))))
            .clone()  // 克隆 Arc
    };

    // 注册工具（每个会话独立的读记录 + console 目录）
    let read_tracker: ReadTracker = Arc::new(Mutex::new(HashSet::new()));
    let tools: Vec<ToolDef> = toolcall::all_tools(
        tool_history.clone(),
        read_tracker,
        state.task_queue.clone(),
        Some(ctx.session_dir.clone()),
        Default::default(),
    );

    // ── 自动任务包装：无活跃任务时，把用户消息自动包装为 task ──
    if !state.task_queue.has_active() {
        let msg_preview: String = req.message.chars().take(10).collect();
        let task_id = format!("处理用户消息：{}", msg_preview);
        let description = &req.message;
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
        context.push(asst_msg);

        // 执行 add_task
        if let Ok(outcome) = toolcall::execute_tool(
            &tools, "add_task",
            serde_json::json!({"id": task_id, "description": description}),
        ).await {
            context.push(Message::new_tool_result(&add_tc_id, &outcome.summary));
        }

        // 执行 start_task
        if let Ok(outcome) = toolcall::execute_tool(
            &tools, "start_task",
            serde_json::json!({"task_id": task_id, "description": description}),
        ).await {
            context.push(Message::new_tool_result(&start_tc_id, &outcome.summary));
        }

        eprintln!("[auto-task] 自动包装为任务: {task_id}");
    }

    // 如果该 session 已有活跃运行，先停止旧标志
    {
        let mut runs = state.active_runs.lock().await;
        if let Some(old) = runs.remove(&session_id) {
            old.signal_stop();
            eprintln!("[chat] session={} 已有活跃 agent，已发停止信号",
                &session_id[..8.min(session_id.len())]);
        }
    }

    // 创建运行标志（stop / pause）并注册到 active_runs
    let flags = Arc::new(RunFlags::new());
    {
        let mut runs = state.active_runs.lock().await;
        runs.insert(session_id.clone(), flags.clone());
    }
    eprintln!("[chat] session={} 注册运行标志", &session_id[..8.min(session_id.len())]);

    // 启动 agent 循环（传入 session_dir 用于读写 CONTEXT.md）
    let warmup_enabled = state.warmup_enabled.load(std::sync::atomic::Ordering::Relaxed);
    let agent_stream = run_agent(
        state.llm.clone_for_agent(),
        tools,
        context,
        system.clone(),
        tool_history,
        Arc::clone(&state.db),
        session_id.clone(),
        Some(ctx.session_dir.clone()),
        state.tool_delay_ms.load(std::sync::atomic::Ordering::Relaxed),
        warmup_enabled,
        flags,
        state.task_queue.clone(),
        state.agent_contexts.clone(),
        state.active_runs.clone(),
    );

    // 将 AgentEvent 转换为 SSE Event
    let sse_stream = agent_to_sse(agent_stream, session_id.clone(), is_new_session);

    // 包装流：SSE 断开时不清理 active_runs（agent 继续在后台运行）
    let sse_stream = CleanupStream {
        inner: sse_stream,
        session_id: session_id.clone(),
    };

    Ok(Sse::new(sse_stream))
}

/// 将 AgentEvent 流转换为 SSE 事件流
fn agent_to_sse(
    agent_stream: ReceiverStream<AgentEvent>,
    _session_id: String,
    _is_new_session: bool,
) -> Pin<Box<dyn Stream<Item = Result<Event, axum::Error>> + Send>> {
    Box::pin(async_stream::stream! {
        use tokio_stream::StreamExt;
        let mut stream = agent_stream;

        while let Some(event) = stream.next().await {
            match event {
                AgentEvent::Session(id) => {
                    yield Ok(Event::default().data(
                        serde_json::to_string(&SseEvent::Session { id }).unwrap()
                    ));
                }
                AgentEvent::Text(t) => {
                    yield Ok(Event::default().data(
                        serde_json::to_string(&SseEvent::Text { content: t }).unwrap()
                    ));
                }
                AgentEvent::Reasoning(r) => {
                    yield Ok(Event::default().data(
                        serde_json::to_string(&SseEvent::Reasoning { content: r }).unwrap()
                    ));
                }
                AgentEvent::ToolCall { id, name, args, result } => {
                    yield Ok(Event::default().data(
                        serde_json::to_string(&SseEvent::ToolCall { id, name, args, result }).unwrap()
                    ));
                }
                AgentEvent::Usage(u) => {
                    yield Ok(Event::default().data(
                        serde_json::to_string(&SseEvent::Usage(u)).unwrap()
                    ));
                }
                AgentEvent::Error(e) => {
                    yield Ok(Event::default().data(
                        serde_json::to_string(&SseEvent::Error { message: e }).unwrap()
                    ));
                }
                AgentEvent::ContextRollback => {
                    yield Ok(Event::default().data(
                        serde_json::to_string(&SseEvent::ContextRollback).unwrap()
                    ));
                }
                AgentEvent::Paused => {
                    yield Ok(Event::default().data(
                        serde_json::to_string(&SseEvent::Paused).unwrap()
                    ));
                }
                AgentEvent::Resumed => {
                    yield Ok(Event::default().data(
                        serde_json::to_string(&SseEvent::Resumed).unwrap()
                    ));
                }
                AgentEvent::MessageBoundary => {
                    yield Ok(Event::default().data(
                        serde_json::to_string(&SseEvent::MessageBoundary).unwrap()
                    ));
                }
            }
        }
    })
}

/// 获取当前设置（API key 返回掩盖版本）
async fn handle_get_settings(
    State(state): State<Arc<AppState>>,
) -> Result<Json<Settings>, (StatusCode, String)> {
    let api_key = state.llm.api_key_snapshot();
    // 掩盖：只显示前4位+后4位
    let masked = api_key.as_ref().map(|k| {
        if k.len() > 8 {
            format!("{}...{}", &k[..4], &k[k.len()-4..])
        } else {
            "****".to_string()
        }
    });
    let system_prompt = state.system_prompt.lock().ok().and_then(|sp| sp.clone());
    let tool_delay_ms = state.tool_delay_ms.load(std::sync::atomic::Ordering::Relaxed);
    let warmup_enabled = state.warmup_enabled.load(std::sync::atomic::Ordering::Relaxed);
    eprintln!("[GET /settings] api_key={:?} system_prompt={:?} tool_delay_ms={} warmup_enabled={}",
        masked.as_deref().unwrap_or("(none)"),
        system_prompt.as_deref().unwrap_or("(none)"),
        tool_delay_ms,
        warmup_enabled);
    Ok(Json(Settings { api_key: masked, system_prompt, tool_delay_ms, warmup_enabled }))
}

/// 更新设置
async fn handle_put_settings(
    State(state): State<Arc<AppState>>,
    Json(update): Json<SettingsUpdate>,
) -> Result<Json<Settings>, (StatusCode, String)> {
    eprintln!("[PUT /settings] api_key={:?} system_prompt={:?} tool_delay_ms={:?}",
        update.api_key.as_ref().map(|_| "(provided)"),
        update.system_prompt.as_deref(),
        update.tool_delay_ms,
    );

    // 更新 API key（如果提供了）
    if let Some(ref key) = update.api_key {
        if !key.is_empty() {
            state.llm.update_api_key(key.clone());
            // 持久化到 DB
            let db = state.db.lock().await;
            let _ = db.set_setting("api_key", key);
            eprintln!("[PUT /settings] API key 已更新");
        }
    }
    // 更新 system prompt
    if let Some(ref sp) = update.system_prompt {
        {
            let mut sys = state.system_prompt.lock().map_err(|e| {
                (StatusCode::INTERNAL_SERVER_ERROR, format!("锁错误: {e}"))
            })?;
            *sys = if sp.is_empty() { None } else { Some(sp.clone()) };
        } // StdMutexGuard 在这里释放
        // 持久化到 DB
        let db = state.db.lock().await;
        if sp.is_empty() {
            let _ = db.delete_setting("system_prompt");
            eprintln!("[PUT /settings] system prompt 已清除");
        } else {
            let _ = db.set_setting("system_prompt", sp);
            eprintln!("[PUT /settings] system prompt 已保存");
        }
    }
    // 更新 tool delay
    if let Some(delay) = update.tool_delay_ms {
        state.tool_delay_ms.store(delay, std::sync::atomic::Ordering::Relaxed);
        let db = state.db.lock().await;
        let _ = db.set_setting("tool_delay_ms", &delay.to_string());
        eprintln!("[PUT /settings] tool_delay_ms 已更新: {}ms", delay);
    }
    // 更新 warmup 开关
    if let Some(enabled) = update.warmup_enabled {
        state.warmup_enabled.store(enabled, std::sync::atomic::Ordering::Relaxed);
        let db = state.db.lock().await;
        let _ = db.set_setting("warmup_enabled", &(enabled as u8).to_string());
        eprintln!("[PUT /settings] warmup_enabled 已更新: {enabled}");
    }

    // 返回当前设置
    let api_key = state.llm.api_key_snapshot();
    let masked = api_key.as_ref().map(|k| {
        if k.len() > 8 {
            format!("{}...{}", &k[..4], &k[k.len()-4..])
        } else {
            "****".to_string()
        }
    });
    let system_prompt = state.system_prompt.lock().ok().and_then(|sp| sp.clone());
    let tool_delay_ms = state.tool_delay_ms.load(std::sync::atomic::Ordering::Relaxed);
    let warmup_enabled = state.warmup_enabled.load(std::sync::atomic::Ordering::Relaxed);
    Ok(Json(Settings { api_key: masked, system_prompt, tool_delay_ms, warmup_enabled }))
}

/// 列出所有会话
async fn handle_list_sessions(
    State(state): State<Arc<AppState>>,
) -> Result<Json<Vec<Session>>, (StatusCode, String)> {
    let db = state.db.lock().await;
    let sessions = db.list_sessions().map_err(|e| {
        (StatusCode::INTERNAL_SERVER_ERROR, format!("DB error: {e}"))
    })?;
    Ok(Json(sessions))
}

/// 获取会话的累计用量
async fn handle_session_usage(
    State(state): State<Arc<AppState>>,
    Path(id): Path<String>,
) -> Result<Json<Option<silences_core::TokenUsage>>, (StatusCode, String)> {
    let db = state.db.lock().await;
    let usage = db.get_total_usage(&id).map_err(|e| {
        (StatusCode::INTERNAL_SERVER_ERROR, format!("DB error: {e}"))
    })?;
    Ok(Json(usage))
}

/// 获取会话的消息历史
async fn handle_session_messages(
    State(state): State<Arc<AppState>>,
    Path(id): Path<String>,
) -> Result<Json<Vec<ViewMessage>>, (StatusCode, String)> {
    let db = state.db.lock().await;
    let msgs = db.get_messages(&id).map_err(|e| {
        (StatusCode::INTERNAL_SERVER_ERROR, format!("DB error: {e}"))
    })?;
    // 预处理为前端可直接渲染的格式（嵌入 tool_results，过滤 tool 角色消息）
    Ok(Json(messages_to_view(msgs)))
}

/// 给上下文消息中的 tool result 填充 name（通过 tool_call_id 反向匹配函数名）
fn enrich_tool_names(msgs: &mut [Message]) {
    let mut tool_names: HashMap<String, String> = HashMap::new();
    for msg in msgs.iter() {
        if let Some(ref tc) = msg.tool_calls {
            for call in tc {
                tool_names.insert(call.id.clone(), call.function.name.clone());
            }
        }
    }
    for msg in msgs.iter_mut() {
        if msg.role == "tool" {
            if let Some(ref id) = msg.tool_call_id {
                if let Some(name) = tool_names.get(id) {
                    msg.name = Some(name.clone());
                }
            }
        }
    }
}

/// 获取会话当前运行时状态（上下文快照 + 任务队列）
async fn handle_session_state(
    State(state): State<Arc<AppState>>,
    Path(id): Path<String>,
) -> Result<Json<SessionState>, (StatusCode, String)> {
    let context = {
        let map = state.agent_contexts.lock().await;
        map.get(&id).cloned()
    };
    // memory 中没有则从 DB 读取（刷新页面后 fallback）
    let mut context = match context {
        Some(c) => c,
        None => {
            let db = state.db.lock().await;
            db.get_context_snapshot(&id).map_err(|e| {
                (StatusCode::INTERNAL_SERVER_ERROR, format!("DB error: {e}"))
            })?.unwrap_or_default()
        }
    };
    // 给 tool result 填函数名，前端无需再计算
    enrich_tool_names(&mut context);
    let tasks = state.task_queue.list();
    // 查询当前 agent 运行状态
    let status = {
        let runs = state.active_runs.lock().await;
        if let Some(f) = runs.get(&id) {
            if f.should_pause() { "paused".to_string() } else { "running".to_string() }
        } else {
            "idle".to_string()
        }
    };
    eprintln!("[GET /sessions/{}/state] context={} tasks={} status={}",
        &id[..8.min(id.len())], context.len(), tasks.len(), status);
    Ok(Json(SessionState { context, tasks, status }))
}

/// 重命名会话
async fn handle_rename_session(
    State(state): State<Arc<AppState>>,
    Path(id): Path<String>,
    Json(req): Json<RenameRequest>,
) -> Result<Json<()>, (StatusCode, String)> {
    let db = state.db.lock().await;
    db.rename_session(&id, &req.name).map_err(|e| {
        (StatusCode::INTERNAL_SERVER_ERROR, format!("DB error: {e}"))
    })?;
    eprintln!("[RENAME] session={} name={:?}", &id[..8.min(id.len())], req.name);
    Ok(Json(()))
}

/// 删除会话
async fn handle_delete_session(
    State(state): State<Arc<AppState>>,
    Path(id): Path<String>,
) -> Result<Json<()>, (StatusCode, String)> {
    let db = state.db.lock().await;
    db.delete_session(&id).map_err(|e| {
        (StatusCode::INTERNAL_SERVER_ERROR, format!("DB error: {e}"))
    })?;
    // 清理 agent 历史
    let mut histories = state.agent_histories.lock().await;
    histories.remove(&id);
    // 清理上下文快照
    let mut ctx = state.agent_contexts.lock().await;
    ctx.remove(&id);
    // 删除会话上下文文件
    if let Some(ref root) = state.project_root {
        silences_agent::context::delete_session_context(root, &id);
    }
    eprintln!("[DELETE] session={}", &id[..8.min(id.len())]);
    Ok(Json(()))
}

/// 设置 agent 运行状态（暂停 / 继续 / 停止）
async fn handle_set_state(
    State(state): State<Arc<AppState>>,
    Path(id): Path<String>,
    Json(req): Json<SetStateRequest>,
) -> Result<Json<()>, (StatusCode, String)> {
    match req.action.as_str() {
        "pause" => {
            let runs = state.active_runs.lock().await;
            if let Some(f) = runs.get(&id) {
                f.signal_pause();
                eprintln!("[SET_STATE] session={} 暂停", &id[..8.min(id.len())]);
                Ok(Json(()))
            } else {
                Err((StatusCode::BAD_REQUEST, "没有正在运行的 agent".into()))
            }
        }
        "resume" => {
            let runs = state.active_runs.lock().await;
            if let Some(f) = runs.get(&id) {
                f.signal_resume();
                eprintln!("[SET_STATE] session={} 恢复", &id[..8.min(id.len())]);
                Ok(Json(()))
            } else {
                Err((StatusCode::BAD_REQUEST, "没有正在运行的 agent".into()))
            }
        }
        "stop" => {
            let mut runs = state.active_runs.lock().await;
            if let Some(f) = runs.remove(&id) {
                f.signal_stop();
                eprintln!("[SET_STATE] session={} 停止", &id[..8.min(id.len())]);
                Ok(Json(()))
            } else {
                eprintln!("[SET_STATE] session={} 无正在运行的 agent", &id[..8.min(id.len())]);
                Ok(Json(())) // 幂等
            }
        }
        _ => Err((StatusCode::BAD_REQUEST, format!("未知动作: {}", req.action))),
    }
}
