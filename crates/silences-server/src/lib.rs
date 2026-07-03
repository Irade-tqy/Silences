//! Silences 后端服务
//!
//! 提供 `POST /chat` 端点，接收用户消息，启动 agent 循环，
//! 以 SSE 流式返回文本回复 + tool call 摘要 + token 用量 + 会话 ID。

use std::collections::{HashMap, HashSet};
use std::path::PathBuf;
use std::pin::Pin;
use std::sync::{Arc, Mutex as StdMutex};

use axum::{
    Json, Router,
    extract::{Path, State},
    http::StatusCode,
    response::sse::{Event, Sse},
    routing::{delete, get, post, put},
};
use futures_util::stream::Stream;
use silences_agent::agent::{run_agent, AgentEvent};
use silences_agent::toolcall::regret::ToolHistory;
use silences_agent::toolcall::{self, ReadTracker, ToolDef};
use silences_core::{ChatRequest, Message, Session, Settings, SettingsUpdate, SseEvent};
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

/// 审批请求
#[derive(serde::Deserialize)]
struct ApproveRequest {
    /// 审批会话 ID（由 present_task_list 生成）
    #[allow(dead_code)]
    approval_id: String,
    /// 目标会话 ID
    session_id: String,
    /// 是否批准
    approved: bool,
    /// 驳回时的反馈（approved=false 时可选）
    feedback: Option<String>,
}

/// 应用状态
struct AppState {
    llm: LlmClient,
    db: Arc<Mutex<Db>>,
    /// 每个会话的 agent 工具历史（用于 regret）
    agent_histories: Mutex<HashMap<String, Arc<Mutex<ToolHistory>>>>,
    /// 最大上下文消息数（对话历史窗口）
    max_context_messages: usize,
    /// 当前设置的 system prompt（运行时可变）
    system_prompt: StdMutex<Option<String>>,
    /// 项目根目录（用于读取 SILENCES.md / CONTEXT.md）
    project_root: Option<PathBuf>,
}

/// 启动服务
pub async fn serve(
    llm: LlmClient,
    db: Db,
    bind: &str,
    max_context_messages: usize,
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

    let project_root = project_root.or_else(silences_agent::context::find_project_root);

    let state = Arc::new(AppState {
        llm,
        db: Arc::new(Mutex::new(db)),
        agent_histories: Mutex::new(HashMap::new()),
        max_context_messages,
        system_prompt: StdMutex::new(saved_system),
        project_root,
    });

    let app = Router::new()
        .route("/chat", post(handle_chat))
        .route("/approve", post(handle_approve))
        .route("/sessions", get(handle_list_sessions))
        .route("/sessions/{id}/messages", get(handle_session_messages))
        .route("/sessions/{id}/usage", get(handle_session_usage))
        .route("/sessions/{id}/rename", put(handle_rename_session))
        .route("/sessions/{id}", delete(handle_delete_session))
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
        db.create_session().map_err(|e| {
            (StatusCode::INTERNAL_SERVER_ERROR, format!("DB error: {e}"))
        })?
    };

    // 保存用户消息
    {
        let db = state.db.lock().await;
        let msg = Message::new_user("user", &req.message);
        db.save_message(&session_id, &msg).map_err(|e| {
            (StatusCode::INTERNAL_SERVER_ERROR, format!("DB error: {e}"))
        })?;
    }

    // 加载历史消息（上下文窗口）
    let mut context = {
        let db = state.db.lock().await;
        db.get_messages(&session_id).map_err(|e| {
            (StatusCode::INTERNAL_SERVER_ERROR, format!("DB error: {e}"))
        })?
    };

    // 截断到 max_context_messages（最新的 N 条）
    if context.len() > state.max_context_messages {
        context = context[context.len() - state.max_context_messages..].to_vec();
    }

    // 读取 SILENCES.md 和 CONTEXT.md
    let ctx = state.project_root.as_ref()
        .map(|root| silences_agent::context::load_project_context(Some(root)))
        .unwrap_or_else(|| silences_agent::context::load_project_context(None));

    // 构建 warmup 前缀消息 = A(历史) + u_user + SILENCES.md
    let mut warmup_msgs = context.clone();
    warmup_msgs.push(Message::new_user("user", &req.message));
    if let Some(ref silences) = ctx.silences_md {
        warmup_msgs.push(Message::new_user("system", silences));
    }

    // 预热稳定前缀（异步发送，不阻塞）
    if warmup_msgs.len() > 2 && ctx.silences_md.is_some() {
        let llm = state.llm.clone_for_agent();
        let sys = system.clone();
        tokio::spawn(async move {
            if let Err(e) = llm.warmup_prefix(&warmup_msgs, sys.as_deref()).await {
                eprintln!("[warmup] 失败: {e}");
            }
        });
    }

    // 将 SILENCES.md / CONTEXT.md 注入到 context 中（用于 agent 的实际请求）
    if let Some(ref silences) = ctx.silences_md {
        context.push(Message::new_user("system", silences));
    }
    if let Some(ref delta) = ctx.context_delta {
        context.push(Message::new_user("system", delta));
    }

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

    // 注册工具（每个会话独立的读记录）
    let read_tracker: ReadTracker = Arc::new(Mutex::new(HashSet::new()));
    let tools: Vec<ToolDef> = toolcall::all_tools(tool_history.clone(), read_tracker);

    // 启动 agent 循环
    let agent_stream = run_agent(
        state.llm.clone_for_agent(),
        tools,
        context,
        system.clone(),
        tool_history,
        Arc::clone(&state.db),
        session_id.clone(),
    );

    // 将 AgentEvent 转换为 SSE Event
    let sse_stream = agent_to_sse(agent_stream, session_id.clone(), is_new_session);

    Ok(Sse::new(sse_stream))
}

/// 处理审批请求
async fn handle_approve(
    State(state): State<Arc<AppState>>,
    Json(req): Json<ApproveRequest>,
) -> Result<Sse<impl Stream<Item = Result<Event, axum::Error>>>, (StatusCode, String)> {
    // 检查 API key
    if state.llm.api_key_snapshot().map_or(true, |k| k.is_empty()) {
        return Err((StatusCode::BAD_REQUEST,
            "请先在设置页面中配置 API Key".to_string()));
    }

    let system = state.system_prompt.lock().ok().and_then(|sp| sp.clone());

    // 加载历史消息
    let mut context = {
        let db = state.db.lock().await;
        db.get_messages(&req.session_id).map_err(|e| {
            (StatusCode::INTERNAL_SERVER_ERROR, format!("DB error: {e}"))
        })?
    };

    // 注入审批结果
    if req.approved {
        context.push(Message::new_user("orch", "审批通过，按任务列表开始执行"));
        eprintln!("[approve] {} 审批通过", &req.session_id[..8.min(req.session_id.len())]);
    } else {
        let feedback = req.feedback.as_deref().unwrap_or("请重新拆分为更合理的任务");
        context.push(Message::new_user("orch", &format!("用户驳回了任务列表：{}。请重新调查并拆分任务。", feedback)));
        eprintln!("[approve] {} 驳回: {}", &req.session_id[..8.min(req.session_id.len())], feedback);
    }

    // 截断到 max_context_messages
    if context.len() > state.max_context_messages {
        context = context[context.len() - state.max_context_messages..].to_vec();
    }

    // SILENCES.md / CONTEXT.md 注入
    let ctx = state.project_root.as_ref()
        .map(|root| silences_agent::context::load_project_context(Some(root)))
        .unwrap_or_else(|| silences_agent::context::load_project_context(None));

    if let Some(ref silences) = ctx.silences_md {
        context.push(Message::new_user("system", silences));
    }
    if let Some(ref delta) = ctx.context_delta {
        context.push(Message::new_user("system", delta));
    }

    // 获取工具历史
    let tool_history = {
        let mut histories = state.agent_histories.lock().await;
        histories
            .entry(req.session_id.clone())
            .or_insert_with(|| Arc::new(Mutex::new(ToolHistory::new(5))))
            .clone()
    };
    let read_tracker: ReadTracker = Arc::new(Mutex::new(HashSet::new()));
    let tools: Vec<ToolDef> = toolcall::all_tools(tool_history.clone(), read_tracker);

    // 预热稳定前缀
    let mut warmup_msgs = context.clone();
    // 截断 warmup 消息到 SILENCES.md 为止（只保留稳定前缀）
    let warmup_end = warmup_msgs.iter().rposition(|m| m.name.as_deref() == Some("system")).map(|i| i + 1).unwrap_or(warmup_msgs.len());
    warmup_msgs.truncate(warmup_end);
    if warmup_msgs.len() > 2 {
        let llm = state.llm.clone_for_agent();
        let sys = system.clone();
        tokio::spawn(async move {
            if let Err(e) = llm.warmup_prefix(&warmup_msgs, sys.as_deref()).await {
                eprintln!("[warmup/approve] 失败: {e}");
            }
        });
    }

    // 启动 agent
    let agent_stream = run_agent(
        state.llm.clone_for_agent(),
        tools,
        context,
        system.clone(),
        tool_history,
        Arc::clone(&state.db),
        req.session_id.clone(),
    );

    let sse_stream = agent_to_sse(agent_stream, req.session_id.clone(), false);
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
                AgentEvent::ToolCalling { name, args } => {
                    yield Ok(Event::default().data(
                        serde_json::to_string(&SseEvent::ToolCalling { name, args }).unwrap()
                    ));
                }
                AgentEvent::ToolResult { name, summary } => {
                    yield Ok(Event::default().data(
                        serde_json::to_string(&SseEvent::ToolResult { name, summary }).unwrap()
                    ));
                }
                AgentEvent::PendingApproval { tasks, approval_id } => {
                    yield Ok(Event::default().data(
                        serde_json::to_string(&SseEvent::PendingApproval { tasks, approval_id }).unwrap()
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
    eprintln!("[GET /settings] api_key={:?} system_prompt={:?}", masked.as_deref().unwrap_or("(none)"), system_prompt.as_deref().unwrap_or("(none)"));
    Ok(Json(Settings { api_key: masked, system_prompt }))
}

/// 更新设置
async fn handle_put_settings(
    State(state): State<Arc<AppState>>,
    Json(update): Json<SettingsUpdate>,
) -> Result<Json<Settings>, (StatusCode, String)> {
    eprintln!("[PUT /settings] api_key={:?} system_prompt={:?}",
        update.api_key.as_ref().map(|_| "(provided)"),
        update.system_prompt.as_deref(),
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
    Ok(Json(Settings { api_key: masked, system_prompt }))
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
) -> Result<Json<Vec<Message>>, (StatusCode, String)> {
    let db = state.db.lock().await;
    let msgs = db.get_messages(&id).map_err(|e| {
        (StatusCode::INTERNAL_SERVER_ERROR, format!("DB error: {e}"))
    })?;
    Ok(Json(msgs))
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
    eprintln!("[DELETE] session={}", &id[..8.min(id.len())]);
    Ok(Json(()))
}
