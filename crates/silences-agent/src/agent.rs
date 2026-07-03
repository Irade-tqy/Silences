//! Agent 循环：LLM ↔ 工具调度 ↔ 流式输出

use std::collections::BTreeMap;
use std::path::PathBuf;
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

/// Agent 产生的对外事件
#[derive(Debug, Clone)]
pub enum AgentEvent {
    /// Session ID（新建会话时）
    Session(String),
    Text(String),
    Reasoning(String),
    ToolCalling {
        name: String,
        args: String,
    },
    ToolResult {
        name: String,
        summary: String,
    },
    /// 等待用户审批任务列表
    PendingApproval {
        /// 任务列表 JSON 字符串
        tasks: String,
        /// 审批会话 ID
        approval_id: String,
    },
    Usage(TokenUsage),
    /// 上下文回退通知（前端应关闭当前消息，开启新空消息）
    ContextRollback,
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

        for round in 0..usize::MAX {
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
            if client_disconnected {
                let (content, tc, rc) = (
                    std::mem::take(&mut full_text),
                    std::mem::take(&mut tc_accums),
                    std::mem::take(&mut full_reasoning),
                );
                let db_lock = db.lock().await;
                if tc.is_empty() {
                    if !content.is_empty() || !rc.is_empty() {
                        let mut m = Message::new("assistant", &content);
                        if !rc.is_empty() { m.reasoning_content = Some(rc); }
                        let _ = db_lock.save_message(&session_id, &m);
                    }
                } else {
                    let tc_values: Vec<ToolCallValue> = tc.values()
                        .map(|tc| ToolCallValue {
                            id: tc.id.clone().unwrap_or_else(|| format!("call_{}", round)),
                            call_type: "function".into(),
                            function: ToolCallFunction {
                                name: tc.name.clone().unwrap_or_default(),
                                arguments: tc.arguments.clone(),
                            },
                        })
                        .collect();
                    let mut m = Message::new_tool_call(tc_values);
                    if !content.is_empty() { m.content = content; }
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

                        // 1. 重放所有累积摘要（assistant, 无 name），前移 checkpoint 保护它们
                        for s in &summaries {
                            messages.push(Message {
                                role: "assistant".into(),
                                content: s.clone(),
                                name: None,
                                reasoning_content: None,
                                tool_calls: None,
                                tool_call_id: None,
                            });
                        }
                        checkpoint = messages.len(); // 摘要进入稳定区，下次截断保留

                        // 2. 刷新 B_delta / CONTEXT.md（name 用绝对路径）
                        if let Some(ref session_dir) = session_dir {
                            if let Some(fresh) = context::read_context_md(session_dir) {
                                let ctx_name = session_dir.join("CONTEXT.md").to_string_lossy().to_string();
                                if let Some(pos) = messages.iter().rposition(|m| m.name.as_deref() == Some(&ctx_name)) {
                                    messages[pos].content = fresh;
                                } else {
                                    messages.push(Message::new_user(&ctx_name, &fresh));
                                }
                            }
                        }

                        // 3. push 继续执行
                        messages.push(Message::new_user("orch", "继续执行后续任务。"));

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
            let mut pending_approval: Option<(String, String)> = None;
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

                if tx
                    .send(AgentEvent::ToolCalling {
                        name: name.clone(),
                        args: args_str.clone(),
                    })
                    .await.is_err() { return; }

                let args: Value = match serde_json::from_str(args_str) {
                    Ok(v) => v,
                    Err(e) => {
                        let err = format!("解析 {name} 参数失败: {e}");
                        let _ = tx.send(AgentEvent::Error(err.clone())).await;
                        let err_msg = Message::new_tool_result(&id, &err);
                        {
                            let db_lock = db.lock().await;
                            let _ = db_lock.save_message(&session_id, &err_msg);
                        }
                        messages.push(err_msg);
                        continue;
                    }
                };

                // 审批拦截：start_task/end_task 需要用户先通过 present_task_list 审批
                if name == "start_task" || name == "end_task" {
                    // 模型在 pending_rollback 期间开启了新任务 → 清除延迟回退
                    if pending_rollback {
                        pending_rollback = false;
                        eprintln!("[agent] 清除 pending_rollback（模型已开始新任务 {}）", name);
                    }
                    let approved = messages.iter().any(|m| {
                        m.role == "user"
                            && m.name.as_deref() == Some("orch")
                            && m.content.contains("通过")
                    });
                    if !approved {
                        let err = format!("{name} 需要先通过 present_task_list 审批才能使用（审批消息必须包含「通过」二字）");
                        let _ = tx.send(AgentEvent::Error(err.clone())).await;
                        let err_msg = Message::new_tool_result(&id, &err);
                        {
                            let db_lock = db.lock().await;
                            let _ = db_lock.save_message(&session_id, &err_msg);
                        }
                        messages.push(err_msg);
                        continue;
                    }
                }

                match toolcall::execute_tool(&tools, &name, args).await {
                    Ok(outcome) => {
                        if outcome.rollback {
                            needs_rollback = true;
                        }
                        if outcome.defer_rollback {
                            needs_defer_rollback = true;
                        }
                        if pending_approval.is_none() {
                            if let Some(ref ap) = outcome.approval_pending {
                                pending_approval = Some((outcome.summary.clone(), ap.clone()));
                            }
                        }

                        if tx
                            .send(AgentEvent::ToolResult {
                                summary: outcome.summary.clone(),
                                name: name.clone(),
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
                        let _ = tx.send(AgentEvent::Error(err.clone())).await;
                        let err_msg = Message::new_tool_result(&id, &err);
                        {
                            let db_lock = db.lock().await;
                            let _ = db_lock.save_message(&session_id, &err_msg);
                        }
                        messages.push(err_msg);
                    }
                }
            }

            // 审批退出：展示任务列表后等待用户审批
            if let Some((tasks, approval_id)) = pending_approval {
                let _ = tx.send(AgentEvent::PendingApproval {
                    tasks,
                    approval_id,
                }).await;
                return;
            }

            // 延迟回退（end_task）：已注入 u_orch，下一轮工具执行完再截断
            if needs_defer_rollback {
                pending_rollback = true;
                continue;
            }

            // 普通回退（非 defer）
            if needs_rollback && checkpoint < messages.len() {
                messages.truncate(checkpoint);
                let _ = tx.send(AgentEvent::ContextRollback).await;
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
