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
use silences_agent::agent::{run_agent, AgentEvent, prepare_agent_context};
use silences_agent::context as agent_context;
use silences_agent::surgery;
use silences_agent::toolcall::regret::ToolHistory;
use silences_agent::toolcall::{self, ReadTracker, ToolDef};
use silences_core::{ChatRequest, Message, RunFlags, Session, SessionState, SetStateRequest, Settings, SettingsUpdate, SurgeryRequest, SseEvent, ViewMessage, messages_to_view};
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
    /// 发送消息时自动清理上下文
    auto_collapse_prev: AtomicBool,
    /// 每个会话最后一次发给 LLM 的 messages 快照
    agent_contexts: Arc<Mutex<HashMap<String, Vec<Message>>>>,
    /// 手术刀 wait 状态（每个会话一个，手术刀 Agent ↔ 主 Agent 同步）
    surgery_waits: Arc<Mutex<HashMap<String, Arc<Mutex<Option<surgery::WaitState>>>>>>,
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
    let auto_collapse_prev = db.get_setting("auto_collapse_prev").ok().flatten()
        .and_then(|s| s.parse::<u8>().ok())
        .map(|v| v != 0)
        .unwrap_or(true);

    // 启动时检查之前未完成的 wait
    let pending_waits = db.list_pending_waits().ok().unwrap_or_default();
    if !pending_waits.is_empty() {
        eprintln!("[serve] 发现 {} 个未完成的 wait，已在 DB 中持久化", pending_waits.len());
    }

    let state = Arc::new(AppState {
        llm,
        db: Arc::new(Mutex::new(db)),
        agent_histories: Mutex::new(HashMap::new()),
        active_runs: Arc::new(Mutex::new(HashMap::new())),
        system_prompt: StdMutex::new(saved_system),
        project_root,
        tool_delay_ms: AtomicU64::new(tool_delay_ms),
        warmup_enabled: AtomicBool::new(warmup_enabled),
        auto_collapse_prev: AtomicBool::new(auto_collapse_prev),
        agent_contexts: Arc::new(Mutex::new(HashMap::new())),
        surgery_waits: Arc::new(Mutex::new(HashMap::new())),
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
        .route("/sessions/{id}/surgery", post(handle_surgery))
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

    // 准备上下文（解析 session、保存消息、加载历史 + SILENCES.md）
    let prep = prepare_agent_context(
        &state.db,
        state.project_root.as_deref(),
        req.session_id.clone(),
        &req.message,
        system.as_deref(),
    ).await.map_err(|e| {
        (StatusCode::INTERNAL_SERVER_ERROR, format!("prepare: {e}"))
    })?;

    let session_id = prep.session_id;
    let is_new_session = prep.is_new;
    let context = prep.messages;

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
        Some(prep.session_dir.clone()),
        Default::default(),
    );

    // 如果该 session 已有活跃运行，先停止旧标志
    {
        let mut runs = state.active_runs.lock().await;
        if let Some(old) = runs.remove(&session_id) {
            old.signal_stop();
        }
    }

    // 创建运行标志（stop / pause）并注册到 active_runs
    let flags = Arc::new(RunFlags::new());
    {
        let mut runs = state.active_runs.lock().await;
        runs.insert(session_id.clone(), flags.clone());
    }

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
        Some(prep.session_dir.clone()),
        state.tool_delay_ms.load(std::sync::atomic::Ordering::Relaxed),
        warmup_enabled,
        flags,
        state.agent_contexts.clone(),
        state.active_runs.clone(),
        None,  // 主 Agent 默认不携带 wait 状态，由 handle_surgery 设置
        false,  // 主 Agent 需要持久化到 DB
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
    let masked = mask_api_key(&api_key);
    let system_prompt = state.system_prompt.lock().ok().and_then(|sp| sp.clone());
    let tool_delay_ms = state.tool_delay_ms.load(std::sync::atomic::Ordering::Relaxed);
    let warmup_enabled = state.warmup_enabled.load(std::sync::atomic::Ordering::Relaxed);
    let auto_collapse_prev = state.auto_collapse_prev.load(std::sync::atomic::Ordering::Relaxed);
    Ok(Json(Settings { api_key: masked, system_prompt, tool_delay_ms, warmup_enabled, auto_collapse_prev }))
}

/// 更新设置
async fn handle_put_settings(
    State(state): State<Arc<AppState>>,
    Json(update): Json<SettingsUpdate>,
) -> Result<Json<Settings>, (StatusCode, String)> {
    // 更新 API key（如果提供了）
    if let Some(ref key) = update.api_key {
        if !key.is_empty() {
            state.llm.update_api_key(key.clone());
            // 持久化到 DB
            let db = state.db.lock().await;
            let _ = db.set_setting("api_key", key);
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
        } else {
            let _ = db.set_setting("system_prompt", sp);
        }
    }
    // 更新 tool delay
    if let Some(delay) = update.tool_delay_ms {
        state.tool_delay_ms.store(delay, std::sync::atomic::Ordering::Relaxed);
        let db = state.db.lock().await;
        let _ = db.set_setting("tool_delay_ms", &delay.to_string());
    }
    // 更新 warmup 开关
    if let Some(enabled) = update.warmup_enabled {
        state.warmup_enabled.store(enabled, std::sync::atomic::Ordering::Relaxed);
        let db = state.db.lock().await;
        let _ = db.set_setting("warmup_enabled", &(enabled as u8).to_string());
    }
    // 更新 auto_collapse_prev 开关
    if let Some(enabled) = update.auto_collapse_prev {
        state.auto_collapse_prev.store(enabled, std::sync::atomic::Ordering::Relaxed);
        let db = state.db.lock().await;
        let _ = db.set_setting("auto_collapse_prev", &(enabled as u8).to_string());
    }

    // 返回当前设置
    let api_key = state.llm.api_key_snapshot();
    let masked = mask_api_key(&api_key);
    let system_prompt = state.system_prompt.lock().ok().and_then(|sp| sp.clone());
    let tool_delay_ms = state.tool_delay_ms.load(std::sync::atomic::Ordering::Relaxed);
    let warmup_enabled = state.warmup_enabled.load(std::sync::atomic::Ordering::Relaxed);
    let auto_collapse_prev = state.auto_collapse_prev.load(std::sync::atomic::Ordering::Relaxed);
    Ok(Json(Settings { api_key: masked, system_prompt, tool_delay_ms, warmup_enabled, auto_collapse_prev }))
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
    // 查询当前 agent 运行状态
    let status = {
        let runs = state.active_runs.lock().await;
        if let Some(f) = runs.get(&id) {
            if f.should_pause() { "paused".to_string() } else { "running".to_string() }
        } else {
            "idle".to_string()
        }
    };
    Ok(Json(SessionState { context, status }))
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
                Ok(Json(()))
            } else {
                Err((StatusCode::BAD_REQUEST, "没有正在运行的 agent".into()))
            }
        }
        "resume" => {
            let runs = state.active_runs.lock().await;
            if let Some(f) = runs.get(&id) {
                f.signal_resume();
                Ok(Json(()))
            } else {
                Err((StatusCode::BAD_REQUEST, "没有正在运行的 agent".into()))
            }
        }
        "stop" => {
            let mut runs = state.active_runs.lock().await;
            if let Some(f) = runs.remove(&id) {
                f.signal_stop();
                Ok(Json(()))
            } else {
                Ok(Json(())) // 幂等
            }
        }
        _ => Err((StatusCode::BAD_REQUEST, format!("未知动作: {}", req.action))),
    }
}

/// 处理手术刀 Agent 请求
///
/// 启动一个独立的 Agent 循环，使用全部标准工具 + wait 工具。
/// Agent 的操作对象是 context.json，用标准 read/write 工具直接读写。
/// 每次工具执行后自动同步 context.json → DB。
async fn handle_surgery(
    State(state): State<Arc<AppState>>,
    Path(id): Path<String>,
    Json(req): Json<SurgeryRequest>,
) -> Result<Sse<impl Stream<Item = Result<Event, axum::Error>>>, (StatusCode, String)> {
    // 读取 SILENCES.md
    let session_dir = state.project_root.as_deref()
        .map(|root| agent_context::session_context_dir(root, &id));
    let silences_md = session_dir.as_ref()
        .and_then(|d| agent_context::read_silences_md(d))
        .unwrap_or_default();

    // 构建手术刀 Agent 的消息
    let system_prompt = state.system_prompt.lock().ok()
        .and_then(|sp| sp.clone());
    let messages = surgery::build_surgery_messages(
        system_prompt.as_deref(),
        &silences_md,
        &req.prompt,
    );

    // 获取工具历史
    let tool_history = {
        let mut histories = state.agent_histories.lock().await;
        histories
            .entry(id.clone())
            .or_insert_with(|| Arc::new(Mutex::new(ToolHistory::new(5))))
            .clone()
    };

    // 注册工具（标准工具 + wait）
    let read_tracker: ReadTracker = Arc::new(Mutex::new(HashSet::new()));
    let base_tools = toolcall::all_tools(
        tool_history.clone(),
        read_tracker,
        session_dir.clone(),
        Default::default(),
    );
    // 初始化 surgery_waits 条目
    let wait_state = {
        let mut sw = state.surgery_waits.lock().await;
        sw.entry(id.clone())
            .or_insert_with(|| Arc::new(Mutex::new(None)))
            .clone()
    };
    let tools = surgery::surgery_tools(base_tools, wait_state.clone());

    // 暂停主 Agent
    {
        let runs = state.active_runs.lock().await;
        if let Some(flags) = runs.get(&id) {
            flags.signal_pause();
        }
    }

    // 创建手术刀 Agent 的运行标志
    let flags = Arc::new(RunFlags::new());
    {
        let mut runs = state.active_runs.lock().await;
        runs.insert(id.clone(), flags.clone());
    }

    let warmup_enabled = state.warmup_enabled.load(std::sync::atomic::Ordering::Relaxed);

    // 创建/刷新 context.json（从内存快照或 DB 恢复当前上下文）
    if let Some(ref dir) = session_dir {
        let ctx_path = dir.join("context.json");
        // 优先从内存快照获取，再从 DB 恢复
        let msgs = {
            let ctx_map = state.agent_contexts.lock().await;
            ctx_map.get(&id).cloned()
        };
        let msgs = if let Some(msgs) = msgs {
            msgs
        } else {
            let db = state.db.lock().await;
            db.get_context_snapshot(&id).ok().flatten().unwrap_or_default()
        };

        if !msgs.is_empty() {
            if let Ok(json) = serde_json::to_string_pretty(&msgs) {
                let _ = std::fs::write(&ctx_path, &json);
                eprintln!("[surgery] 已写入 context.json ({} 条消息)", msgs.len());
            }
        }
    }

    // 启动手术刀 Agent 循环（no_db_persist=true 避免写入主会话 DB）
    let agent_stream = run_agent(
        state.llm.clone_for_agent(),
        tools,
        messages,
        system_prompt.clone(),
        tool_history,
        Arc::clone(&state.db),
        id.clone(),
        session_dir.clone(),
        state.tool_delay_ms.load(std::sync::atomic::Ordering::Relaxed),
        warmup_enabled,
        flags,
        state.agent_contexts.clone(),
        state.active_runs.clone(),
        None,  // 手术刀 Agent 自身不检查 wait（wait 由主 Agent 检查）
        true,  // no_db_persist: 手术刀模式不写主会话 DB
    );

    // 包装流：工具执行后同步 context.json
    let sse_stream = surgery_agent_to_sse(
        agent_stream, id.clone(), session_dir, state.db.clone(), state.agent_contexts.clone()
    );

    let sse_stream = CleanupStream {
        inner: sse_stream,
        session_id: id.clone(),
    };

    Ok(Sse::new(sse_stream))
}

/// 将手术刀 Agent 的事件流转换为 SSE 事件流，并在工具执行后同步 context.json
fn surgery_agent_to_sse(
    agent_stream: ReceiverStream<AgentEvent>,
    session_id: String,
    session_dir: Option<PathBuf>,
    db: Arc<Mutex<Db>>,
    agent_contexts: Arc<Mutex<HashMap<String, Vec<Message>>>>,
) -> Pin<Box<dyn Stream<Item = Result<Event, axum::Error>> + Send>> {
    Box::pin(async_stream::stream! {
        use tokio_stream::StreamExt;
        let mut stream = agent_stream;

        while let Some(event) = stream.next().await {
            match event {
                AgentEvent::ToolCall { ref name, ref result, .. } => {
                    // 工具执行完成后，同步 context.json
                    if result.is_some() && name != "wait" {
                        if let Some(ref dir) = session_dir {
                            sync_context_json(dir, &session_id, &db, &agent_contexts);
                        }
                    }
                    yield Ok(Event::default().data(
                        serde_json::to_string(&SseEvent::ToolCall {
                            id: String::new(),
                            name: name.clone(),
                            args: String::new(),
                            result: result.clone(),
                        }).unwrap()
                    ));
                }
                AgentEvent::Text(_) | AgentEvent::Reasoning(_) => {
                    // 前端不渲染 text/reasoning
                }
                AgentEvent::Session(s) => {
                    yield Ok(Event::default().data(
                        serde_json::to_string(&SseEvent::Session { id: s }).unwrap()
                    ));
                }
                AgentEvent::Usage(u) => {
                    yield Ok(Event::default().data(
                        serde_json::to_string(&SseEvent::Usage(u)).unwrap()
                    ));
                }
                AgentEvent::MessageBoundary => {
                    yield Ok(Event::default().data(
                        serde_json::to_string(&SseEvent::MessageBoundary).unwrap()
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
                AgentEvent::Error(e) => {
                    yield Ok(Event::default().data(
                        serde_json::to_string(&SseEvent::Error { message: e }).unwrap()
                    ));
                }
            }
        }
    })
}

/// 同步 context.json 到 DB + 内存快照
fn sync_context_json(
    session_dir: &std::path::Path,
    session_id: &str,
    db: &Arc<Mutex<Db>>,
    agent_contexts: &Arc<Mutex<HashMap<String, Vec<Message>>>>,
) {
    let ctx_path = session_dir.join("context.json");
    if !ctx_path.exists() {
        return;
    }

    // 1. 读取
    let content = match std::fs::read_to_string(&ctx_path) {
        Ok(c) => c,
        Err(e) => {
            eprintln!("[surgery] 读取 context.json 失败: {e}");
            return;
        }
    };
    let raw: Vec<serde_json::Value> = match serde_json::from_str(&content) {
        Ok(v) => v,
        Err(e) => {
            eprintln!("[surgery] 解析 context.json 失败: {e}");
            return;
        }
    };

    // 2. 标准化
    let normalized = surgery::normalize_messages(raw);

    // 3. 写回 context.json（标准化后的版本）
    if let Ok(json) = serde_json::to_string_pretty(&normalized) {
        let _ = std::fs::write(&ctx_path, &json);
    }

    // 4. 同步到 DB + 内存快照
    let rt = tokio::runtime::Handle::current();
    rt.block_on(async {
        let db_lock = db.lock().await;
        // 删除 session 的所有旧消息
        // 注意：不能直接 delete_all，需要保留其他方式插入的消息
        // 更安全的做法：只更新 context_snapshot，因为 context.json 不一定是 message 表的一致副本
        let _ = db_lock.save_context_snapshot(session_id, &normalized);

        let mut ctx_map = agent_contexts.lock().await;
        ctx_map.insert(session_id.to_string(), normalized);
    });
}

/// 掩盖 API key：只显示前4位+后4位
fn mask_api_key(key: &Option<String>) -> Option<String> {
    key.as_ref().map(|k| {
        if k.len() > 8 {
            format!("{}...{}", &k[..4], &k[k.len()-4..])
        } else {
            "****".to_string()
        }
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::body::to_bytes;
    use axum::response::IntoResponse;
    use silences_core::{ToolCallFunction, ToolCallValue};

    // ── mask_api_key ──

    #[test]
    fn test_mask_api_key_long() {
        let key = Some("sk-ant-1234567890abc".to_string());
        assert_eq!(mask_api_key(&key), Some("sk-a...0abc".to_string()));
    }

    #[test]
    fn test_mask_api_key_short() {
        let key = Some("short".to_string());
        assert_eq!(mask_api_key(&key), Some("****".to_string()));
    }

    #[test]
    fn test_mask_api_key_none() {
        assert_eq!(mask_api_key(&None), None);
    }

    #[test]
    fn test_mask_api_key_exactly_eight() {
        let key = Some("12345678".to_string());
        assert_eq!(mask_api_key(&key), Some("****".to_string()));
    }

    // ── enrich_tool_names ──

    #[test]
    fn test_enrich_tool_names_basic() {
        let mut msgs = vec![
            Message::new_tool_call(vec![ToolCallValue {
                id: "call_1".into(),
                call_type: "function".into(),
                function: ToolCallFunction {
                    name: "search".into(),
                    arguments: "{}".into(),
                },
            }]),
            Message::new_tool_result("call_1", "some result"),
        ];
        enrich_tool_names(&mut msgs);
        assert_eq!(msgs[1].name.as_deref(), Some("search"));
    }

    #[test]
    fn test_enrich_tool_names_noop() {
        let mut msgs = vec![
            Message::new("user", "hello"),
            Message::new("assistant", "hi there"),
        ];
        enrich_tool_names(&mut msgs);
        assert!(msgs[0].name.is_none());
        assert!(msgs[1].name.is_none());
    }

    #[test]
    fn test_enrich_tool_names_missing_id() {
        let mut msgs = vec![
            Message::new_tool_call(vec![ToolCallValue {
                id: "call_1".into(),
                call_type: "function".into(),
                function: ToolCallFunction {
                    name: "search".into(),
                    arguments: "{}".into(),
                },
            }]),
            Message::new_tool_result("call_2", "result"),
        ];
        enrich_tool_names(&mut msgs);
        // No matching tool_call_id → name stays None
        assert!(msgs[1].name.is_none());
    }

    #[test]
    fn test_enrich_tool_names_empty() {
        let mut msgs: Vec<Message> = vec![];
        // Should not panic
        enrich_tool_names(&mut msgs);
    }

    #[test]
    fn test_enrich_tool_names_multiple() {
        let mut msgs = vec![
            Message::new_tool_call(vec![
                ToolCallValue {
                    id: "c1".into(),
                    call_type: "function".into(),
                    function: ToolCallFunction {
                        name: "search".into(),
                        arguments: "{}".into(),
                    },
                },
                ToolCallValue {
                    id: "c2".into(),
                    call_type: "function".into(),
                    function: ToolCallFunction {
                        name: "read".into(),
                        arguments: "{}".into(),
                    },
                },
            ]),
            Message::new_tool_result("c1", "result1"),
            Message::new_tool_result("c2", "result2"),
        ];
        enrich_tool_names(&mut msgs);
        assert_eq!(msgs[1].name.as_deref(), Some("search"));
        assert_eq!(msgs[2].name.as_deref(), Some("read"));
    }

    #[test]
    fn test_enrich_tool_names_interleaved() {
        let mut msgs = vec![
            Message::new_tool_call(vec![ToolCallValue {
                id: "c1".into(),
                call_type: "function".into(),
                function: ToolCallFunction {
                    name: "search".into(),
                    arguments: "{}".into(),
                },
            }]),
            Message::new_tool_result("c1", "result"),
            Message::new("assistant", "based on search..."),
            Message::new_tool_call(vec![ToolCallValue {
                id: "c2".into(),
                call_type: "function".into(),
                function: ToolCallFunction {
                    name: "read_file".into(),
                    arguments: "{}".into(),
                },
            }]),
            Message::new_tool_result("c2", "content"),
        ];
        enrich_tool_names(&mut msgs);
        assert_eq!(msgs[1].name.as_deref(), Some("search"));
        assert_eq!(msgs[4].name.as_deref(), Some("read_file"));
    }

    // ── agent_to_sse ──

    #[tokio::test]
    async fn test_agent_to_sse_events() {
        let (tx, rx) = tokio::sync::mpsc::channel(16);
        tx.send(AgentEvent::Text("hello".into())).await.unwrap();
        tx.send(AgentEvent::Session("sess_1".into())).await.unwrap();
        tx.send(AgentEvent::Reasoning("thinking...".into())).await.unwrap();
        tx.send(AgentEvent::ToolCall {
            id: "call_1".into(),
            name: "search".into(),
            args: "{}".into(),
            result: Some("results".into()),
        })
        .await
        .unwrap();
        tx.send(AgentEvent::MessageBoundary).await.unwrap();
        drop(tx); // close channel so stream ends

        let stream = agent_to_sse(ReceiverStream::new(rx), "sess_1".into(), false);
        let response = Sse::new(stream).into_response();
        let bytes = to_bytes(response.into_body(), 10000).await.unwrap();
        let body = String::from_utf8_lossy(&bytes);

        // SSE wire format: "data: <json>\n\n"
        let parts: Vec<&str> = body.split("\n\n").filter(|s| !s.is_empty()).collect();
        assert_eq!(parts.len(), 5, "expected 5 SSE events, got {}: {body:?}", parts.len());

        let prefix = "data: ";
        let events: Vec<SseEvent> = parts
            .iter()
            .map(|p| {
                let trimmed = p.trim();
                assert!(
                    trimmed.starts_with(prefix),
                    "expected 'data: ' prefix, got: {trimmed:?}"
                );
                let json = &trimmed[prefix.len()..];
                serde_json::from_str(json).expect("failed to parse SseEvent JSON")
            })
            .collect();

        assert!(matches!(&events[0], SseEvent::Text { content } if content == "hello"));
        assert!(matches!(&events[1], SseEvent::Session { id } if id == "sess_1"));
        assert!(matches!(&events[2], SseEvent::Reasoning { content } if content == "thinking..."));
        assert!(matches!(
            &events[3],
            SseEvent::ToolCall { id, name, .. } if id == "call_1" && name == "search"
        ));
        assert!(matches!(&events[4], SseEvent::MessageBoundary));
    }
}
