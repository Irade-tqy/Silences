//! Agent 循环：LLM ↔ 工具调度 ↔ 流式输出

use std::collections::BTreeMap;
use std::sync::Arc;

use serde_json::Value;
use silences_core::{Message, TokenUsage, ToolCallValue, ToolCallFunction};
use silences_llm::{LlmClient, StreamDelta};
use silences_db::Db;
use tokio::sync::Mutex;
use tokio_stream::wrappers::ReceiverStream;
use crate::toolcall::regret::ToolHistory;
use crate::toolcall::{self, ToolDef};

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
pub fn run_agent(
    llm: LlmClient,
    tools: Vec<ToolDef>,
    mut messages: Vec<Message>,
    system: Option<String>,
    tool_history: Arc<Mutex<ToolHistory>>,
    db: Arc<Mutex<Db>>,
    session_id: String,
) -> ReceiverStream<AgentEvent> {
    let (tx, rx) = tokio::sync::mpsc::channel(64);

    tokio::spawn(async move {
        // 发射 Session 事件
        if tx.send(AgentEvent::Session(session_id.clone())).await.is_err() {
            return;
        }

        // 初始 checkpoint = 传入 messages 的长度
        let checkpoint = messages.len();

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
                    // 保存到 DB
                    {
                        let db_lock = db.lock().await;
                        let _ = db_lock.save_message(&session_id, &final_asst);
                    }
                }
                if let Some(u) = usage {
                    let _ = tx.send(AgentEvent::Usage(u.clone())).await;
                    {
                        let db_lock = db.lock().await;
                        let _ = db_lock.save_usage(&session_id, round as u32, &u);
                    }
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
            // 保存 assistant 消息
            {
                let db_lock = db.lock().await;
                let _ = db_lock.save_message(&session_id, &asst_msg);
            }
            messages.push(asst_msg);

            // 逐个执行工具
            let mut needs_rollback = false;
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

                // 通知前端
                if tx
                    .send(AgentEvent::ToolCalling {
                        name: name.clone(),
                        args: args_str.clone(),
                    })
                    .await.is_err() { return; }

                // 解析参数
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

                // 执行工具（regret 现在由工具自身的 handler 处理）
                match toolcall::execute_tool(&tools, &name, args).await {
                    Ok(outcome) => {
                        if outcome.rollback {
                            needs_rollback = true;
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

                        // 记录逆操作（command 不可撤销）
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

            // 若工具要求审批，发送审批事件并退出
            if let Some((tasks, approval_id)) = pending_approval {
                let _ = tx.send(AgentEvent::PendingApproval {
                    tasks,
                    approval_id,
                }).await;
                // 退出 agent 循环，等待前端审批
                return;
            }

            // 若工具要求回退，截断消息到 checkpoint
            if needs_rollback && checkpoint < messages.len() {
                messages.truncate(checkpoint);
                let _ = tx.send(AgentEvent::Text(
                    "\n\n[上下文已回退，开始下一任务]".into()
                )).await;
            }

            // 继续下一轮（LLM 看到工具结果后继续）
        }
    });

    ReceiverStream::new(rx)
}

