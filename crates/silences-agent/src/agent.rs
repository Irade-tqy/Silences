//! Agent 循环：LLM ↔ 工具调度 ↔ 流式输出

use std::collections::BTreeMap;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::Duration;

use serde_json::Value;
use silences_core::{Message, TokenUsage, ToolCallValue, ToolCallFunction};
use silences_llm::{LlmClient, StreamDelta};
use silences_db::Db;
use tokio::sync::Mutex;
use tokio_stream::wrappers::ReceiverStream;
use crate::toolcall::regret::ToolHistory;
use crate::toolcall::{self, ToolDef};
use crate::context;
use crate::queue::TaskQueue;

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
    Error(String),
}

/// 累积中的 tool call
#[derive(Default)]
struct AccumTc {
    id: Option<String>,
    name: Option<String>,
    arguments: String,
}

/// 运行 agent 循环，返回事件流
///
/// agent 自行管理 LLM↔tool 循环，产生流式事件供后端转发 SSE。
/// 同时将消息和用量持久化到数据库。
/// `tool_history` 跨多次 agent run 共享（同 session 的撤回链）。
/// `checkpoint` 是 messages 的回退点索引，[0..checkpoint) 稳定不变。
/// `session_dir` 是会话的上下文目录（.silences/sessions/{id}），用于读写 CONTEXT.md。
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
    stop_flag: Arc<AtomicBool>,
    queue: Arc<TaskQueue>,
) -> ReceiverStream<AgentEvent> {
    let (tx, rx) = tokio::sync::mpsc::channel(64);

    tokio::spawn(async move {
        // 发射 Session 事件
        if tx.send(AgentEvent::Session(session_id.clone())).await.is_err() {
            return;
        }

        // 初始 checkpoint = 传入 messages 的长度（前移以保护摘要不截断）
        let mut checkpoint = messages.len();
        // 延迟回退标志 + 累积摘要
        let mut pending_rollback = false;
        let mut summaries: Vec<String> = Vec::new();
        // 保留边界：此 ID 之前的消息不被 rollback 隐藏，随边界推进不断扩大保留区
        let mut last_preserved_id: i64 = {
            let db_lock = db.lock().await;
            db_lock.get_max_message_id(&session_id).unwrap_or(None).unwrap_or(0)
        };
        eprintln!("[agent] last_preserved_id={last_preserved_id} checkpoint={checkpoint}");

        for round in 0..usize::MAX {
            // 检查外部停止信号
            if stop_flag.load(Ordering::Relaxed) {
                eprintln!("[agent] 收到停止信号，退出 agent 循环");
                let _ = tx.send(AgentEvent::Error("已停止".into())).await;
                return;
            }
            let api_tools = toolcall::build_api_tools(&tools);

            // 调用 LLM（流式）
            let mut stream = match llm
                .chat_stream(&messages, system.as_deref(), Some(api_tools))
                .await
            {
                Ok(s) => s,
                Err(e) => {
                    let _ = tx.send(AgentEvent::Error(format!("LLM 调用失败: {e}"))).await;
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
                        if tx.send(AgentEvent::Text(t)).await.is_err() {
                            client_disconnected = true;
                            break;
                        }
                    }
                    Ok(Some(StreamDelta::Reasoning(r))) => {
                        full_reasoning.push_str(&r);
                        if tx.send(AgentEvent::Reasoning(r)).await.is_err() {
                            client_disconnected = true;
                            break;
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
                        return;
                    }
                }
            }

            // 客户端断开：保存已积累的内容再退出
            //
            // 注意：如果断开时 LLM 正在流式输出 tool_call，这些 tool_call 尚未执行，
            // 我们只保存已积累的文本/推理内容，不保存 tool_calls —— 否则会在 DB 中
            // 留下孤立 tool_calls（有 tool_calls 但无对应 tool_result），
            // 导致下次 API 请求因「工具调用没闭合」而返回 400。
            if client_disconnected {
                let (content, _tc, rc) = (
                    std::mem::take(&mut full_text),
                    std::mem::take(&mut tc_accums),
                    std::mem::take(&mut full_reasoning),
                );
                let db_lock = db.lock().await;
                if !content.is_empty() || !rc.is_empty() {
                    let mut m = Message::new("assistant", &content);
                    if !rc.is_empty() { m.reasoning_content = Some(rc); }
                    let _ = db_lock.save_message(&session_id, &m);
                }
                return;
            }

            // 获取 usage
            if let Some(u) = stream.take_usage() {
                usage = Some(u);
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
                }

                // 保存用量
                if let Some(ref u) = usage {
                    let _ = tx.send(AgentEvent::Usage(u.clone())).await;
                    {
                        let db_lock = db.lock().await;
                        let _ = db_lock.save_usage(&session_id, round as u32, u);
                    }
                }

                // 检查是否有待处理的延迟回退（end_task → u_orch → 模型更新 CONTEXT.md 后 stop）
                if pending_rollback {
                    pending_rollback = false;

                    // 捕获本轮总结，累积不截断
                    let round_summary = full_text.clone();
                    if !round_summary.is_empty() {
                        summaries.push(round_summary);
                    }

                    if checkpoint < messages.len() {
                        messages.truncate(checkpoint);

                        // 隐藏本轮产生的工具消息，保留 last_preserved_id 之前的消息
                        let db_lock = db.lock().await;
                        let _ = db_lock.hide_messages_after(&session_id, last_preserved_id);

                        // 1. 重放所有累积摘要到内存和 DB（name 留空，前台显示为普通 assistant）
                        for s in &summaries {
                            let msg = Message {
                                role: "assistant".into(),
                                content: s.clone(),
                                name: None,
                                reasoning_content: None,
                                tool_calls: None,
                                tool_call_id: None,
                            };
                            let _ = db_lock.save_message(&session_id, &msg);
                            messages.push(msg);
                        }
                        checkpoint = messages.len(); // 摘要进入稳定区，下次截断保留

                        // ⚡ 预热下一轮稳定前缀 [SILENCES.md, user, ...summaries]
                        if let Err(e) = llm.warmup_prefix(&messages[..checkpoint], system.as_deref()).await {
                            eprintln!("[agent] warmup 失败: {e}");
                        }
                        tokio::time::sleep(std::time::Duration::from_secs(1)).await; // 等缓存稳定

                        // 2. 刷新 B_delta / CONTEXT.md（name 用绝对路径）到内存和 DB
                        if let Some(ref session_dir) = session_dir {
                            if let Some(fresh) = context::read_context_md(session_dir) {
                                let ctx_name = session_dir.join("CONTEXT.md").to_string_lossy().to_string();
                                if let Some(pos) = messages.iter().rposition(|m| m.name.as_deref() == Some(&ctx_name)) {
                                    messages[pos].content = fresh.clone();
                                } else {
                                    messages.push(Message::new_user(&ctx_name, &fresh));
                                }
                                // CONTEXT.md 也持久化到 DB
                                let _ = db_lock.save_message(&session_id, &Message::new_user(&ctx_name, &fresh));
                            }
                        }

                        // 推进保留边界，使刚写入的摘要 + CONTEXT.md 不被下次 rollback 隐藏
                        if let Ok(Some(new_id)) = db_lock.get_max_message_id(&session_id) {
                            last_preserved_id = new_id;
                        }
                        drop(db_lock);

                        // 3. 检查队列状态，决定继续还是请求总结
                        if queue.is_empty() {
                            messages.push(Message::new_user("orch", "所有任务已完成。请生成一份全面的最终总结，然后结束。"));
                            eprintln!("[agent] 队列已空，请求最终总结");
                        } else {
                            messages.push(Message::new_user("orch", "继续执行后续任务。"));
                        }

                        let _ = tx.send(AgentEvent::ContextRollback).await;
                    }
                    continue;
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

            // 保存本轮 usage（tool call 轮次的 API 用量）
            if let Some(ref u) = usage {
                let _ = tx.send(AgentEvent::Usage(u.clone())).await;
                {
                    let db_lock = db.lock().await;
                    let _ = db_lock.save_usage(&session_id, round as u32, u);
                }
            }

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
                if tx
                    .send(AgentEvent::ToolCall {
                        id: id.clone(),
                        name: name.clone(),
                        args: args_str.clone(),
                        result: None,
                    })
                    .await.is_err() { return; }

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
                        continue;
                    }
                };

                // 执行 tool 前检查停止信号
                if stop_flag.load(Ordering::Relaxed) {
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
                    break;
                }

                match toolcall::execute_tool(&tools, &name, args).await {
                    Ok(outcome) => {
                        if outcome.rollback {
                            needs_rollback = true;
                        }
                        if outcome.defer_rollback {
                            needs_defer_rollback = true;
                        }

                        // 发射完成状态的 tool call
                        if tx
                            .send(AgentEvent::ToolCall {
                                id: id.clone(),
                                name: name.clone(),
                                args: args_str.clone(),
                                result: Some(outcome.summary.clone()),
                            })
                            .await.is_err() { return; }

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

                        // 注入额外消息（如 end_task 注入 u_orch）
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
                    }
                }
            }

            // 延迟回退（end_task）：已注入 u_orch，下一轮工具执行完再截断
            if needs_defer_rollback {
                pending_rollback = true;
                let _ = tx.send(AgentEvent::MessageBoundary).await;
                continue;
            }

            // 普通回退（非 defer）
            let mut did_rollback = false;
            if needs_rollback && checkpoint < messages.len() {
                messages.truncate(checkpoint);
                // 隐藏 DB 中本轮产生的消息
                {
                    let db_lock = db.lock().await;
                    let _ = db_lock.hide_messages_after(&session_id, last_preserved_id);
                }
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

            // 继续下一轮（LLM 看到工具结果后继续）
        }
    });

    ReceiverStream::new(rx)
}
