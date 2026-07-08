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
use crate::checkpoint_stack::CheckpointStack;
use crate::context;

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
    /// 上下文回退通知（前端应关闭当前消息，开启新空消息）
    ContextRollback,
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
) -> bool {
    let mut was_paused = false;
    while flags.should_pause() {
        if !was_paused {
            let _ = tx.send(AgentEvent::Paused).await;
            was_paused = true;
        }
        tokio::time::sleep(Duration::from_millis(500)).await;
        if flags.should_stop() {
            // 停止时保存最终快照
            {
                let mut map = agent_contexts.lock().await;
                map.insert(session_id.to_string(), messages.to_vec());
            }
            {
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
/// `checkpoint` 是 messages 的回退点索引，[0..checkpoint) 稳定不变。
/// `session_dir` 是会话的上下文目录（.silences/sessions/{id}），用于读写 CONTEXT.md。
/// `active_runs` 用于 agent 自然退出时清理停止标志（保持不因 SSE 断开而清理）。
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
    warmup_enabled: bool,
    flags: Arc<RunFlags>,
    cp_stack: Arc<CheckpointStack>,
    agent_contexts: Arc<Mutex<HashMap<String, Vec<Message>>>>,
    active_runs: Arc<Mutex<HashMap<String, Arc<RunFlags>>>>,
) -> ReceiverStream<AgentEvent> {
    let (tx, rx) = tokio::sync::mpsc::channel(64);

    tokio::spawn(async move {
        // 发射 Session 事件
        if tx.send(AgentEvent::Session(session_id.clone())).await.is_err() {
            let mut runs = active_runs.lock().await;
            runs.remove(&session_id);
            return;
        }

        let _ = &cp_stack; // cp_stack 通过工具间接使用

        // 初始 checkpoint = 传入 messages 的长度（稳定区边界，永不移动）
        let checkpoint = messages.len();
        // 延迟回退标志 + 回退目标检查点 ID + 是否为自动检查点
        let mut pending_rollback = false;
        let mut rollback_target_cp: Option<String> = None;
        let mut rollback_is_auto = false;

        // 累计用量（所有轮次叠加后发给前端，让 cost 面板实时显示）
        let mut total_usage: Option<TokenUsage> = None;

        // 构建一次 API 工具定义（每轮复用，只 clone）
        let api_tools = toolcall::build_api_tools(&tools);
        // 上下文快照条件写入标志
        let mut messages_changed = true;

        for round in 0..usize::MAX {
            let t_round = std::time::Instant::now();
            // 快照当前上下文供 /state 端点查询（仅在 messages 有变化时写入）
            if messages_changed {
                {
                    let mut map = agent_contexts.lock().await;
                    map.insert(session_id.clone(), messages.clone());
                }
                // 持久化到 DB，刷新页面后仍可恢复
                {
                    let db_lock = db.lock().await;
                    let _ = db_lock.save_context_snapshot(&session_id, &messages);
                }
                messages_changed = false;
            }

            // 暂停等待循环（每轮开始时检查，等待外部 resume 或 stop）
            if flags.should_pause() {
                if pause_until_resumed(&tx, &flags, &messages, &session_id,
                    &agent_contexts, &db, &active_runs).await {
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
                    &agent_contexts, &db, &active_runs).await {
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
                    {
                        let db_lock = db.lock().await;
                        let _ = db_lock.save_message(&session_id, &final_asst);
                    }
                    messages.push(final_asst);
                    messages_changed = true;
                }

                // 保存用量（发送累计值）
                accumulate_usage(&usage, &mut total_usage, &tx, &db, &session_id, round).await;

                // 检查是否有待处理的延迟回滚
                if pending_rollback {
                    pending_rollback = false;

                    // 获取回滚目标检查点 ID（pre-check 已确保存在）
                    let target_cp = match rollback_target_cp.take() {
                        Some(id) => id,
                        None => continue,
                    };
                    let is_auto = rollback_is_auto;
                    rollback_is_auto = false; // 复位

                    // 在 messages 中找到目标检查点的 tool_result 位置（或自动检查点的消息索引）
                    let cp_end = if is_auto {
                        // 自动检查点：使用存储的消息位置索引
                        cp_stack.get_auto_msg_index(&target_cp)
                    } else {
                        let mut found = None;
                        'outer: for (i, m) in messages.iter().enumerate() {
                            if let Some(tcs) = &m.tool_calls {
                                for tc in tcs {
                                    if tc.function.name != "checkpoint" { continue; }
                                    let matches = serde_json::from_str::<serde_json::Value>(&tc.function.arguments)
                                        .ok()
                                        .and_then(|v| v.get("id").and_then(|v| v.as_str().map(String::from)))
                                        == Some(target_cp.clone());
                                    if matches {
                                        for j in (i+1)..messages.len() {
                                            if messages[j].tool_call_id.as_deref() == Some(&tc.id) {
                                                found = Some(j + 1);
                                                break 'outer;
                                            }
                                        }
                                        found = Some(i + 2);
                                        break 'outer;
                                    }
                                }
                            }
                        }
                        found
                    };

                    // 保存 rollback 工具调用记录（截断前复制）
                    let rollback_tc = messages.iter().rfind(|m| {
                        m.tool_calls.as_ref().map_or(false, |tcs| {
                            tcs.iter().any(|tc| tc.function.name == "rollback")
                        })
                    }).cloned();
                    let rb_call_id: Option<String> = rollback_tc.as_ref()
                        .and_then(|m| m.tool_calls.as_ref())
                        .and_then(|tcs| tcs.first())
                        .map(|tc| tc.id.clone());
                    let rollback_tr = rb_call_id.as_ref().and_then(|call_id| {
                        messages.iter().rfind(|m| {
                            m.role == "tool" && m.tool_call_id.as_deref() == Some(call_id)
                        }).cloned()
                    });
                    let round_summary = full_text.clone();

                    // 预检已在 tool 执行阶段确保检查点存在，此处不应失败
                    let Some(cp_end) = cp_end else {
                        eprintln!("[agent] 警告：检查点 \"{target_cp}\" 在 pending_rollback 中未找到截断位置（可能是重启后丢失了自动检查点的消息索引）");
                        // 兜底：不清除消息但继续执行（通知 LLM）
                        let fallback_outcome = Message::new_tool_result(
                            &rollback_tc.as_ref()
                                .and_then(|m| m.tool_calls.as_ref())
                                .and_then(|tcs| tcs.first())
                                .map(|tc| tc.id.clone())
                                .unwrap_or_default(),
                            &format!("⚠️ 检查点 \"{target_cp}\" 存在但无法确定截断位置（会话重启后自动检查点索引丢失）。请使用 checkpoint 工具创建新检查点后重试。"),
                        );
                        {
                            let db_lock = db.lock().await;
                            let _ = db_lock.save_message(&session_id, &fallback_outcome);
                        }
                        messages.push(fallback_outcome);
                        messages_changed = true;
                        continue;
                    };

                    // 截断到目标检查点之后（保留 checkpoint 记录）
                    messages.truncate(cp_end);
                    messages_changed = true;

                    let db_lock = db.lock().await;

                    // 1. 本轮总结（checkpoint TR ↔ rollback TC 之间）
                    if !round_summary.is_empty() {
                        let summary_msg = Message {
                            role: "assistant".into(),
                            content: round_summary,
                            name: None,
                            reasoning_content: None,
                            tool_calls: None,
                            tool_call_id: None,
                        };
                        let _ = db_lock.save_message(&session_id, &summary_msg);
                        messages.push(summary_msg);
                        messages_changed = true;
                    }

                    // 2. rollback 工具记录（保留心智模型）
                    if let Some(tc) = rollback_tc {
                        messages.push(tc);
                        messages_changed = true;
                    }
                    if let Some(mut tr) = rollback_tr {
                        // 阶段 2：覆写 TR 内容，替换"回滚中"为"已回滚" + checkpoint 列表
                        tr.content = format!(
                            "✅ 已回滚到检查点 `{target_cp}`。\n\n当前检查点：\n{}",
                            cp_stack.format_for_context()
                        );
                        messages.push(tr);
                        messages_changed = true;
                    }

                    // 3. CONTEXT.md（rollback TR 之后，role: system）
                    if let Some(ref session_dir) = session_dir {
                        if let Some(fresh) = context::read_context_md(session_dir) {
                            let now = chrono::Utc::now().format("%Y-%m-%d %H:%M:%S UTC");
                            let ctx_path = session_dir.join("CONTEXT.md").to_string_lossy().to_string();
                            let system_content = format!("<!-- 更新于 {now} -->\n{fresh}");
                            let ctx_msg = Message {
                                role: "system".into(),
                                content: system_content,
                                name: Some(ctx_path),
                                reasoning_content: None,
                                tool_calls: None,
                                tool_call_id: None,
                            };
                            let _ = db_lock.save_message(&session_id, &ctx_msg);
                            messages.push(ctx_msg);
                            messages_changed = true;
                        }
                    }
                    drop(db_lock);

                    // checkpoint 不变：稳定区始终是初始位置
                    // ⚡ 预热稳定前缀
                    if warmup_enabled && checkpoint < messages.len() {
                        if let Err(e) = llm.warmup_prefix(&messages[..checkpoint], system.as_deref()).await {
                            eprintln!("[agent] 警告: warmup 失败: {e}");
                        }
                    }

                    let _ = tx.send(AgentEvent::ContextRollback).await;
                    continue;
                }
                // 正常退出前最后一次快照（仅在 messages 有变化时写入）
                if messages_changed {
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
                let _ = db_lock.save_message(&session_id, &asst_msg);
            }
            messages.push(asst_msg);
            messages_changed = true;

            // 保存本轮 usage（tool call 轮次的 API 用量，发送累计值）
            accumulate_usage(&usage, &mut total_usage, &tx, &db, &session_id, round).await;

            // 逐个执行工具
            let mut needs_rollback = false;
            let mut needs_defer_rollback = false;
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
                        {
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
                    {
                        let db_lock = db.lock().await;
                        let _ = db_lock.save_message(&session_id, &err_msg);
                    }
                    messages.push(err_msg);
                    messages_changed = true;
                    break;
                }

                // 捕获 rollback 目标检查点 ID（在 args 被 move 前），同时预检存在性
                let mut rollback_precheck_failed: Option<String> = None;
                if name == "rollback" {
                    if let Some(cp_id) = args.get("checkpoint_id").and_then(|v| v.as_str()) {
                        rollback_target_cp = Some(cp_id.to_string());
                        // 预检：检查点是否在 cp_stack 中存在（覆盖自动 + 用户创建）
                        let in_stack = cp_stack.list().iter().any(|c| c.id == cp_id);
                        if !in_stack {
                            rollback_precheck_failed = Some(cp_id.to_string());
                        } else {
                            // 判断是否为自动检查点（不在消息历史中）
                            let in_messages = messages.iter().any(|m| {
                                m.tool_calls.as_ref().map_or(false, |tcs| {
                                    tcs.iter().any(|tc| {
                                        tc.function.name == "checkpoint"
                                            && serde_json::from_str::<serde_json::Value>(&tc.function.arguments)
                                                .ok().as_ref()
                                                .and_then(|v| v.get("id"))
                                                .and_then(|v| v.as_str())
                                                == Some(cp_id)
                                    })
                                })
                            });
                            rollback_is_auto = !in_messages;
                        }
                    }
                }

                match toolcall::execute_tool(&tools, &name, args).await {
                    Ok(mut outcome) => {
                        // 预检失败：覆盖 outcome 为普通错误（不设 rollback/defer）
                        if let Some(cp_id) = rollback_precheck_failed {
                            outcome.summary = format!("❌ 回滚失败：检查点 \"{cp_id}\" 不存在。请先使用 list_checkpoints 查看可用检查点。");
                            outcome.rollback = false;
                            outcome.defer_rollback = false;
                            outcome.inject_messages = vec![];
                        }

                        if outcome.rollback {
                            needs_rollback = true;
                        }
                        if outcome.defer_rollback {
                            needs_defer_rollback = true;
                        }

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
                        {
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
                            {
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
                        {
                            let db_lock = db.lock().await;
                            let _ = db_lock.save_message(&session_id, &err_msg);
                        }
                        messages.push(err_msg);
                        messages_changed = true;
                    }
                }
            }

            // 延迟回退（rollback）：tool result 已指示 LLM 更新 CONTEXT.md，下一轮再截断
            if needs_defer_rollback {
                pending_rollback = true;
                let _ = tx.send(AgentEvent::MessageBoundary).await;
                continue;
            }

            // 普通回退（非 defer）
            let mut did_rollback = false;
            if needs_rollback && checkpoint < messages.len() {
                messages.truncate(checkpoint);
                messages_changed = true;
                let _ = tx.send(AgentEvent::ContextRollback).await;
                did_rollback = true;
            }

            // 没有 ContextRollback 时发 MessageBoundary，让前端知道新消息开始了
            if !did_rollback {
                let _ = tx.send(AgentEvent::MessageBoundary).await;
            }

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
    cp_stack: Arc<CheckpointStack>,
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
        cp_stack,
        agent_contexts.clone(),
        active_runs,
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
        {
            let db_lock = db.lock().await;
            let _ = db_lock.save_usage(session_id, round as u32, u);
        }
    }
}

/// 在每条用户消息后自动创建检查点（存入 cp_stack + 持久化到 DB + 记录消息位置）
/// `msg_count`：创建时消息总数（含刚添加的用户消息），用于 rollback 截断
/// 不注入任何假消息，模型对此无感知
pub async fn auto_checkpoint(
    cp_stack: &CheckpointStack,
    db: &Arc<Mutex<Db>>,
    session_id: &str,
    message: &str,
    msg_count: usize,
) {
    let ts = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs();
    let id = format!("cp_{:x}", ts);
    let desc: String = message.chars().take(40).collect();

    cp_stack.push_auto(id.clone(), desc.clone(), msg_count);

    // 持久化到 DB，重启后可恢复
    let db_lock = db.lock().await;
    let _ = db_lock.save_checkpoint(session_id, &id, &desc);
}
