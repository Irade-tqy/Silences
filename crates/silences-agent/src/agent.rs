//! Agent 循环：LLM ↔ 工具调度 ↔ 流式输出

use std::collections::{BTreeMap, HashMap};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use serde_json::Value;
use silences_core::{Message, RunFlags, TokenUsage, ToolCallValue, ToolCallFunction};
use silences_llm::{LlmClient, StreamDelta};
use silences_db::Db;
use tokio::sync::Mutex;
use tokio_stream::wrappers::ReceiverStream;
use crate::toolcall::regret::ToolHistory;
use crate::toolcall::{self, ToolDef};
use crate::context;
use crate::surgery;

/// Agent 产生的对外事件
#[derive(Debug, Clone)]
pub enum AgentEvent {
    /// Session ID（新建会话时）
    Session(String),
    Text(String),
    Reasoning(String),
    /// 工具调用（含执行结果，result=None 表示执行中，Some 表示已完成）
    ToolCall {
        id: String,
        name: String,
        args: String,
        result: Option<String>,
    },
    Usage(TokenUsage),
    /// 消息边界：下一轮 LLM 响应是新消息
    MessageBoundary,
    /// agent 已暂停
    Paused,
    /// agent 已恢复
    Resumed,
    Error(String),
}

/// Agent 循环结束后的输出（阻塞式 API 用）
#[derive(Debug, Clone)]
pub struct AgentOutput {
    /// 最终的 messages 快照
    pub messages: Vec<Message>,
    /// 累计用量
    pub total_usage: Option<TokenUsage>,
    /// 最终 assistant 文本回复
    pub assistant_reply: String,
}

/// 准备好的会话上下文（由 prepare_agent_context 返回）
#[derive(Debug, Clone)]
pub struct PreparedContext {
    pub session_id: String,
    pub messages: Vec<Message>,
    pub session_dir: PathBuf,
    pub is_new: bool,
}

/// 累积中的 tool call
#[derive(Default)]
struct AccumTc {
    id: Option<String>,
    name: Option<String>,
    arguments: String,
}

/// 暂停等待循环：发送 Paused 事件，每 500ms 轮询 resume/stop 标志
///
/// 返回 `true` = 暂停期间收到停止信号 → 调用者应 return（agent 已清理）
/// 返回 `false` = 正常恢复
async fn pause_until_resumed(
    tx: &tokio::sync::mpsc::Sender<AgentEvent>,
    flags: &RunFlags,
    messages: &[Message],
    session_id: &str,
    agent_contexts: &tokio::sync::Mutex<HashMap<String, Vec<Message>>>,
    db: &tokio::sync::Mutex<Db>,
    active_runs: &tokio::sync::Mutex<HashMap<String, Arc<RunFlags>>>,
    no_db_persist: bool,
) -> bool {
    let mut was_paused = false;
    while flags.should_pause() {
        if !was_paused {
            let _ = tx.send(AgentEvent::Paused).await;
            was_paused = true;
        }
        tokio::time::sleep(Duration::from_millis(500)).await;
        if flags.should_stop() {
            // 停止时保存最终快照（手术刀模式跳过）
            if !no_db_persist {
                let mut map = agent_contexts.lock().await;
                map.insert(session_id.to_string(), messages.to_vec());
            }
            if !no_db_persist {
                let db_lock = db.lock().await;
                let _ = db_lock.save_context_snapshot(session_id, messages);
            }
            let _ = tx.send(AgentEvent::Error("已停止".into())).await;
            {
                let mut runs = active_runs.lock().await;
                runs.remove(session_id);
            }
            return true;
        }
    }
    let _ = tx.send(AgentEvent::Resumed).await;
    false
}

/// 运行 agent 循环，返回事件流
///
/// agent 自行管理 LLM↔tool 循环，产生流式事件供后端转发 SSE。
/// 同时将消息和用量持久化到数据库。
/// `tool_history` 跨多次 agent run 共享（同 session 的撤回链）。
/// `session_dir` 是会话的上下文目录（.silences/sessions/{id}），用于读写 CONTEXT.md。
/// `active_runs` 用于 agent 自然退出时清理停止标志（保持不因 SSE 断开而清理）。
/// `surgery_wait` 如果为 Some，则在每轮工具执行后检查 wait 条件是否达成。
/// `no_db_persist` 如果为 true，跳过所有 DB 写入（用于手术刀 Agent，避免写入主会话 DB）。
pub fn run_agent(
    llm: LlmClient,
    tools: Vec<ToolDef>,
    mut messages: Vec<Message>,
    system: Option<String>,
    tool_history: Arc<Mutex<ToolHistory>>,
    db: Arc<Mutex<Db>>,
    session_id: String,
    session_dir: Option<PathBuf>,
    tool_delay_ms: u64,
    _warmup_enabled: bool,
    flags: Arc<RunFlags>,
    agent_contexts: Arc<Mutex<HashMap<String, Vec<Message>>>>,
    active_runs: Arc<Mutex<HashMap<String, Arc<RunFlags>>>>,
    surgery_wait: Option<Arc<Mutex<Option<surgery::WaitState>>>>,
    no_db_persist: bool,
) -> ReceiverStream<AgentEvent> {
    let (tx, rx) = tokio::sync::mpsc::channel(64);

    tokio::spawn(async move {
        // 发射 Session 事件
        if tx.send(AgentEvent::Session(session_id.clone())).await.is_err() {
            let mut runs = active_runs.lock().await;
            runs.remove(&session_id);
            return;
        }

        // 累计用量（所有轮次叠加后发给前端，让 cost 面板实时显示）
        let mut total_usage: Option<TokenUsage> = None;

        // 构建一次 API 工具定义（每轮复用，只 clone）
        let api_tools = toolcall::build_api_tools(&tools);
        // 上下文快照条件写入标志
        let mut messages_changed = true;

        for round in 0..usize::MAX {
            let t_round = std::time::Instant::now();
            // 快照当前上下文供 /state 端点查询（仅在 messages 有变化时写入）
            // 手术刀模式不更新 agent_contexts（由 sync_context_json 管理）
            if messages_changed && !no_db_persist {
                {
                    let mut map = agent_contexts.lock().await;
                    map.insert(session_id.clone(), messages.clone());
                }
                // 持久化到 DB，刷新页面后仍可恢复（手术刀模式跳过）
                if !no_db_persist {
                    let db_lock = db.lock().await;
                    let _ = db_lock.save_context_snapshot(&session_id, &messages);
                }
                messages_changed = false;
            }

            // 暂停等待循环（每轮开始时检查，等待外部 resume 或 stop）
            if flags.should_pause() {
                if pause_until_resumed(&tx, &flags, &messages, &session_id,
                    &agent_contexts, &db, &active_runs, no_db_persist).await {
                    return;
                }
            }

            // 检查外部停止信号
            if flags.should_stop() {
                let _ = tx.send(AgentEvent::Error("已停止".into())).await;
                {
                    let mut runs = active_runs.lock().await;
                    runs.remove(&session_id);
                }
                return;
            }
            // 调用 LLM（流式）
            let mut stream = match llm
                .chat_stream(&messages, system.as_deref(), Some(api_tools.clone()))
                .await
            {
                Ok(s) => s,
                Err(e) => {
                    let _ = tx.send(AgentEvent::Error(format!("LLM 调用失败: {e}"))).await;
                    {
                        let mut runs = active_runs.lock().await;
                        runs.remove(&session_id);
                    }
                    return;
                }
            };

            // 流式解析
            let mut full_text = String::new();
            let mut full_reasoning = String::new();
            let mut tc_accums: BTreeMap<usize, AccumTc> = BTreeMap::new();
            let mut usage: Option<TokenUsage> = None;
            let mut client_disconnected = false;

            loop {
                match stream.next_delta().await {
                    Ok(Some(StreamDelta::Text(t))) => {
                        full_text.push_str(&t);
                        if !client_disconnected && tx.send(AgentEvent::Text(t)).await.is_err() {
                            client_disconnected = true;
                        }
                    }
                    Ok(Some(StreamDelta::Reasoning(r))) => {
                        full_reasoning.push_str(&r);
                        if !client_disconnected && tx.send(AgentEvent::Reasoning(r)).await.is_err() {
                            client_disconnected = true;
                        }
                    }
                    Ok(Some(StreamDelta::ToolCall { index, id, name, arguments })) => {
                        let entry = tc_accums.entry(index).or_default();
                        if let Some(i) = id {
                            entry.id = Some(i);
                        }
                        if let Some(n) = name {
                            entry.name = Some(n);
                        }
                        entry.arguments.push_str(&arguments);
                    }
                    Ok(None) => break,
                    Err(e) => {
                        let _ = tx
                            .send(AgentEvent::Error(format!("流式读取失败: {e}")))
                            .await;
                        {
                            let mut runs = active_runs.lock().await;
                            runs.remove(&session_id);
                        }
                        return;
                    }
                }
            }

            // 获取 usage
            if let Some(u) = stream.take_usage() {
                usage = Some(u);
            }

            let t_llm_done = std::time::Instant::now();
            // LLM 流完成后、处理结果前允许暂停
            // 如果推理期间收到暂停信号，流正常结束然后暂停，不执行工具
            if flags.should_pause() {
                if pause_until_resumed(&tx, &flags, &messages, &session_id,
                    &agent_contexts, &db, &active_runs, no_db_persist).await {
                    return;
                }
            }

            // 判断：有 tool call 则执行
            if tc_accums.is_empty() {
                // 纯文本响应 → 保存消息 + 用量
                if !full_text.is_empty() || !full_reasoning.is_empty() {
                    let mut final_asst = Message::new("assistant", &full_text);
                    if !full_reasoning.is_empty() {
                        final_asst.reasoning_content = Some(full_reasoning);
                    }
                    if !no_db_persist {
                        let db_lock = db.lock().await;
                        let _ = db_lock.save_message(&session_id, &final_asst);
                    }
                    messages.push(final_asst);
                    messages_changed = true;
                }

                // 保存用量（发送累计值）
                accumulate_usage(&usage, &mut total_usage, &tx, &db, &session_id, round, no_db_persist).await;

                // 正常退出前最后一次快照（仅在 messages 有变化时写入）
                if messages_changed && !no_db_persist {
                    {
                        let mut map = agent_contexts.lock().await;
                        map.insert(session_id.clone(), messages.clone());
                    }
                    {
                        let db_lock = db.lock().await;
                        let _ = db_lock.save_context_snapshot(&session_id, &messages);
                    }
                }
                // agent 正常退出，清理 active_runs 中的停止标志
                {
                    let mut runs = active_runs.lock().await;
                    runs.remove(&session_id);
                }
                return;
            }

            // 构建 assistant 消息（含 tool_calls）
            let tc_values: Vec<ToolCallValue> = tc_accums
                .values()
                .map(|tc| ToolCallValue {
                    id: tc.id.clone().unwrap_or_else(|| format!("call_{}", round)),
                    call_type: "function".into(),
                    function: ToolCallFunction {
                        name: tc.name.clone().unwrap_or_default(),
                        arguments: tc.arguments.clone(),
                    },
                })
                .collect();

            let mut asst_msg = Message::new_tool_call(tc_values);
            if !full_text.is_empty() {
                asst_msg.content = full_text;
            }
            if !full_reasoning.is_empty() {
                asst_msg.reasoning_content = Some(full_reasoning);
            }
            {
                let db_lock = db.lock().await;
                if !no_db_persist {
                    let _ = db_lock.save_message(&session_id, &asst_msg);
                }
            }
            messages.push(asst_msg);
            messages_changed = true;

            // 保存本轮 usage（tool call 轮次的 API 用量，发送累计值）
            accumulate_usage(&usage, &mut total_usage, &tx, &db, &session_id, round, no_db_persist).await;

            // 逐个执行工具
            for tc in tc_accums.values() {
                let name = match &tc.name {
                    Some(n) => n.clone(),
                    None => {
                        let _ = tx
                            .send(AgentEvent::Error("工具调用缺少 name".into()))
                            .await;
                        continue;
                    }
                };
                let args_str = &tc.arguments;
                let id = tc
                    .id
                    .clone()
                    .unwrap_or_else(|| format!("call_{}", round));

                // 先发射 pending 状态的 tool call
                if !client_disconnected
                    && tx
                        .send(AgentEvent::ToolCall {
                            id: id.clone(),
                            name: name.clone(),
                            args: args_str.clone(),
                            result: None,
                        })
                        .await.is_err()
                {
                    client_disconnected = true;
                }

                let args: Value = match serde_json::from_str(args_str) {
                    Ok(v) => v,
                    Err(e) => {
                        let err = format!("解析 {name} 参数失败: {e}");
                        let _ = tx.send(AgentEvent::ToolCall {
                            id: id.clone(), name: name.clone(),
                            args: args_str.clone(), result: Some(err.clone()),
                        }).await;
                        let err_msg = Message::new_tool_result(&id, &err);
                        if !no_db_persist {
                            let db_lock = db.lock().await;
                            let _ = db_lock.save_message(&session_id, &err_msg);
                        }
                        messages.push(err_msg);
                        messages_changed = true;
                        continue;
                    }
                };

                // 执行 tool 前检查停止信号
                if flags.should_stop() {
                    let err = "已停止".to_string();
                    let _ = tx.send(AgentEvent::ToolCall {
                        id: id.clone(), name: name.clone(),
                        args: args_str.clone(), result: Some(err.clone()),
                    }).await;
                    let err_msg = Message::new_tool_result(&id, &err);
                    if !no_db_persist {
                        let db_lock = db.lock().await;
                        let _ = db_lock.save_message(&session_id, &err_msg);
                    }
                    messages.push(err_msg);
                    messages_changed = true;
                    break;
                }

                match toolcall::execute_tool(&tools, &name, args).await {
                    Ok(outcome) => {
                        // 发射完成状态的 tool call
                        if !client_disconnected
                            && tx
                                .send(AgentEvent::ToolCall {
                                    id: id.clone(),
                                    name: name.clone(),
                                    args: args_str.clone(),
                                    result: Some(outcome.summary.clone()),
                                })
                                .await.is_err()
                        {
                            client_disconnected = true;
                        }

                        if let Some(inv) = outcome.inverse {
                            let mut history = tool_history.lock().await;
                            history.push(&name, inv);
                        }

                        let tool_msg = Message::new_tool_result(&id, &outcome.summary);
                        if !no_db_persist {
                            let db_lock = db.lock().await;
                            let _ = db_lock.save_message(&session_id, &tool_msg);
                        }
                        messages.push(tool_msg);
                        messages_changed = true;

                        // 注入额外消息（如 rollback 注入 orch）
                        for mut inject_msg in outcome.inject_messages {
                            // 在 orch 指令中嵌入 CONTEXT.md 的绝对路径
                            if inject_msg.name.as_deref() == Some("orch") {
                                if let Some(ref session_dir) = session_dir {
                                    let ctx_path = session_dir.join("CONTEXT.md").to_string_lossy().to_string();
                                    inject_msg.content = inject_msg.content.replace("CONTEXT.md", &ctx_path);
                                }
                            }
                            if !no_db_persist {
                                let db_lock = db.lock().await;
                                let _ = db_lock.save_message(&session_id, &inject_msg);
                            }
                            messages.push(inject_msg);
                            messages_changed = true;
                        }
                    }
                    Err(e) => {
                        let err = format!("{name} 执行失败: {e}");
                        let _ = tx.send(AgentEvent::ToolCall {
                            id: id.clone(), name: name.clone(),
                            args: args_str.clone(), result: Some(err.clone()),
                        }).await;
                        let err_msg = Message::new_tool_result(&id, &err);
                        if !no_db_persist {
                            let db_lock = db.lock().await;
                            let _ = db_lock.save_message(&session_id, &err_msg);
                        }
                        messages.push(err_msg);
                        messages_changed = true;
                    }
                }
            }

            // 发 MessageBoundary，让前端知道新消息开始了
            let _ = tx.send(AgentEvent::MessageBoundary).await;

            // 工具循环延迟（调试用）
            if tool_delay_ms > 0 {
                tokio::time::sleep(Duration::from_millis(tool_delay_ms)).await;
            }

            let t_total = t_round.elapsed();
            let t_llm = t_llm_done - t_round;
            let t_tools = t_total - t_llm;
            eprintln!(
                "[perf] round={round} total={:.1}s llm={:.1}s tools={:.1}s msgs={}",
                t_total.as_secs_f64(),
                t_llm.as_secs_f64(),
                t_tools.as_secs_f64(),
                messages.len(),
            );

            // ── 手术刀 wait 条件检查 ──
            if let Some(ref wait_mutex) = surgery_wait {
                let condition = {
                    let ws = wait_mutex.lock().await;
                    ws.as_ref().map(|w| w.condition.clone())
                };
                if let Some(condition) = condition {
                    let mut check_msgs = messages.clone();
                    check_msgs.push(Message::new_user("orch",
                        &format!("判断条件是否已经完成：{condition}\n只输出 y 或 n。不开思考")));

                    match llm.chat_stream(&check_msgs,
                        Some("只输出 y 或 n。不开思考"), None).await
                    {
                        Ok(mut stream) => {
                            let mut text = String::new();
                            loop {
                                match stream.next_delta().await {
                                    Ok(Some(StreamDelta::Text(t))) => text.push_str(&t),
                                    Ok(Some(_)) => continue,
                                    Ok(None) => break,
                                    Err(_) => break,
                                }
                            }
                            if text.trim().to_lowercase() == "y" {
                                // 条件达成，通知 wait 工具返回
                                let mut ws = wait_mutex.lock().await;
                                if let Some(completer) = ws.as_mut().and_then(|w| w.completer.take()) {
                                    let _ = completer.send(());
                                }
                                *ws = None;
                                flags.signal_pause();
                                {
                                    let db_lock = db.lock().await;
                                    let _ = db_lock.delete_surgery_wait(&session_id);
                                }
                            }
                        }
                        Err(e) => {
                            eprintln!("[agent] wait 条件检查 LLM 调用失败: {e}");
                        }
                    }
                }
            }

            // 继续下一轮（LLM 看到工具结果后继续）
        }
    });

    ReceiverStream::new(rx)
}

/// 阻塞式运行 agent：发送消息 → 等待完成 → 获取回复
///
/// 内部调用 `run_agent()` 但消费完整事件流，组装为 `AgentOutput` 返回。
/// 不依赖外部 pause/stop 信号，适合 lib 模式（AgengBench 等外部调用方）。
pub async fn run_agent_blocking(
    llm: LlmClient,
    tools: Vec<ToolDef>,
    messages: Vec<Message>,
    system: Option<String>,
    tool_history: Arc<Mutex<ToolHistory>>,
    db: Arc<Mutex<Db>>,
    session_id: String,
    session_dir: Option<PathBuf>,
    tool_delay_ms: u64,
    warmup_enabled: bool,
    agent_contexts: Arc<Mutex<HashMap<String, Vec<Message>>>>,
) -> anyhow::Result<AgentOutput> {
    // 创建内部 dummy flags（从不 pause/stop）
    let flags = Arc::new(RunFlags::new());
    let active_runs = Arc::new(Mutex::new(HashMap::new()));
    {
        let mut runs = active_runs.lock().await;
        runs.insert(session_id.clone(), flags.clone());
    }

    let stream = run_agent(
        llm,
        tools,
        messages,
        system,
        tool_history,
        db,
        session_id.clone(),
        session_dir,
        tool_delay_ms,
        warmup_enabled,
        flags,
        agent_contexts.clone(),
        active_runs,
        None,  // 阻塞式 API 不使用 wait
        false, // 阻塞式 API 默认持久化到 DB
    );

    // 消费完整事件流
    let mut assistant_reply = String::new();
    let mut total_usage: Option<TokenUsage> = None;
    let mut error: Option<String> = None;

    use tokio_stream::StreamExt;
    let mut s = stream;
    while let Some(event) = s.next().await {
        match event {
            AgentEvent::Text(t) => {
                assistant_reply.push_str(&t);
            }
            AgentEvent::Usage(u) => {
                total_usage = Some(u);
            }
            AgentEvent::Error(e) => {
                error = Some(e);
            }
            _ => {}
        }
    }

    // 流结束后从 agent_contexts 读取最终 messages
    let messages = {
        let map = agent_contexts.lock().await;
        map.get(&session_id).cloned().unwrap_or_default()
    };

    if let Some(e) = error {
        anyhow::bail!("agent 错误: {e}");
    }

    Ok(AgentOutput {
        messages,
        total_usage,
        assistant_reply,
    })
}

/// 准备 agent 上下文：解析 session、保存用户消息、加载历史 + SILENCES.md
///
/// 从 server `handle_chat()` 中提取的共享逻辑，server 和 lib 均可使用。
pub async fn prepare_agent_context(
    db: &Arc<Mutex<Db>>,
    project_root: Option<&Path>,
    session_id: Option<String>,
    user_message: &str,
    _system: Option<&str>,
) -> anyhow::Result<PreparedContext> {
    // 解析会话 ID
    let is_new = session_id.as_ref().map_or(true, |s| s.is_empty());
    let session_id = if !is_new {
        session_id.unwrap()
    } else {
        let db = db.lock().await;
        let sid = db.create_session()?;
        // 新会话初始化上下文文件
        if let Some(root) = project_root {
            if let Err(e) = context::init_session_context(root, &sid) {
                eprintln!("[agent] 初始化会话上下文失败: {e}");
            }
        }
        sid
    };

    // 保存用户消息
    {
        let db = db.lock().await;
        let msg = Message::new_user("user", user_message);
        db.save_message(&session_id, &msg)?;
    }

    // 加载历史消息
    let history = {
        let db = db.lock().await;
        db.get_messages(&session_id)?
    };

    // 加载 SILENCES.md（role: system）
    let ctx = context::load_project_context(project_root, Some(&session_id));
    let silences_name = ctx.session_dir.join("SILENCES.md").to_string_lossy().to_string();
    let mut messages: Vec<Message> = Vec::new();
    if let Some(ref silences) = ctx.silences_md {
        messages.push(Message {
            role: "system".into(),
            content: silences.clone(),
            name: Some(silences_name),
            reasoning_content: None,
            tool_calls: None,
            tool_call_id: None,
        });
    }
    messages.extend(history);


    Ok(PreparedContext {
        session_id,
        messages,
        session_dir: ctx.session_dir,
        is_new,
    })
}

/// 累计并保存 token 用量（纯文本分支和 tool call 分支共用）
async fn accumulate_usage(
    round_usage: &Option<TokenUsage>,
    total_usage: &mut Option<TokenUsage>,
    tx: &tokio::sync::mpsc::Sender<AgentEvent>,
    db: &Arc<Mutex<Db>>,
    session_id: &str,
    round: usize,
    no_db_persist: bool,
) {
    if let Some(u) = round_usage {
        let total = total_usage.as_ref().map(|t|
            TokenUsage::new(
                t.input_tokens + u.input_tokens,
                t.output_tokens + u.output_tokens,
                t.cache_hit_tokens + u.cache_hit_tokens,
                t.cache_miss_tokens + u.cache_miss_tokens,
            )
        ).unwrap_or_else(|| u.clone());
        *total_usage = Some(total.clone());
        let _ = tx.send(AgentEvent::Usage(total)).await;
        if !no_db_persist {
            let db_lock = db.lock().await;
            let _ = db_lock.save_usage(session_id, round as u32, u);
        }
    }
}

