//! 上下文管理 Agent（侧边栏手术刀）
//!
//! 提供 wait(condition) 工具、context.json 标准化、Agent 消息构建等功能。
//! 复用主 Agent 的 run_agent 循环和全部标准文件工具。

use std::collections::HashSet;
use std::sync::Arc;

use anyhow::{anyhow};
use serde_json::Value;
use silences_core::Message;
use tokio::sync::Mutex;
use tokio::sync::oneshot;

use crate::toolcall::{ToolDef, ToolOutcome};

/// wait 状态：手术刀 Agent → 主 Agent 通信桥梁
///
/// 手术刀 Agent 调用 wait 工具时创建此状态，主 Agent 每轮结束后检查条件，
/// 条件达成后通过 completer 通知 wait 工具返回。
pub struct WaitState {
    pub condition: String,
    pub completer: Option<oneshot::Sender<()>>,
}

/// 构建手术刀 Agent 的消息列表
///
/// 消息结构:
///   [0] system 用户设置的 system prompt（可选）
///   [1] system SILENCES.md（包含 context.json 信息）
///   [2] user   "根据要求修改 context.json，不要管其他文件: " + 用户指令
pub fn build_surgery_messages(
    system_prompt: Option<&str>,
    silences_md: &str,
    user_prompt: &str,
) -> Vec<Message> {
    let mut msgs = Vec::new();

    // 可选 system prompt
    if let Some(sys) = system_prompt {
        msgs.push(Message::new("system", sys));
    }

    // SILENCES.md（已包含 context.json 文件说明）
    msgs.push(Message {
        role: "system".into(),
        content: silences_md.to_string(),
        name: Some("SILENCES.md".into()),
        reasoning_content: None,
        tool_calls: None,
        tool_call_id: None,
    });

    // 用户指令
    msgs.push(Message::new("user",
        &format!("根据要求修改 context.json，不要管其他文件: {}", user_prompt)));

    msgs
}

/// 构建手术刀 Agent 的工具列表
///
/// 复用主 Agent 的全部标准工具，并追加 wait 工具。
pub fn surgery_tools(
    base_tools: Vec<ToolDef>,
    wait_state: Arc<Mutex<Option<WaitState>>>,
) -> Vec<ToolDef> {
    let mut tools = base_tools;
    tools.push(wait_tool(wait_state));
    tools
}

/// wait 工具定义
///
/// 暂停当前上下文操作，让主 Agent 恢复工作，等待条件达成后继续。
/// 不设超时，用户可通过前端取消。
fn wait_tool(wait_state: Arc<Mutex<Option<WaitState>>>) -> ToolDef {
    ToolDef {
        name: "wait",
        description: "暂停当前操作，让主 Agent 恢复工作，等待某个外部条件被满足。\
                      条件满足后继续执行后续操作。\n\
                      参数: condition (必填) — 等待的条件描述，如'代码重构已完成'",
        schema: serde_json::json!({
            "type": "object",
            "properties": {
                "condition": {
                    "type": "string",
                    "description": "等待的条件，如'代码重构已完成'"
                }
            },
            "required": ["condition"],
            "additionalProperties": false
        }),
        handler: Box::new(move |args| {
            let ws = wait_state.clone();
            Box::pin(async move {
                let condition = args["condition"]
                    .as_str()
                    .ok_or_else(|| anyhow!("wait 缺少 condition 参数"))?
                    .to_string();

                let (tx, rx) = oneshot::channel::<()>();

                // 设置 wait 状态（主 Agent 每轮结束后会检测到此状态）
                *ws.lock().await = Some(WaitState {
                    condition: condition.clone(),
                    completer: Some(tx),
                });

                // 无限等待条件达成
                // 用户可通过前端取消（中断 oneshot receiver）
                rx.await.map_err(|_| anyhow!("wait 被中断: {condition}"))?;

                Ok(ToolOutcome::new(format!("条件已达成: {condition}")))
            })
        }),
    }
}

/// 标准化 context.json 中的消息列表
///
/// 修复手术刀 Agent 写入 context.json 时可能产生的脏数据：
/// - content: null → ""
/// - assistant 的 reasoning_content: null → ""
/// - 空 tool_calls 数组 → 移除字段
pub fn normalize_messages(raw: Vec<Value>) -> Vec<Message> {
    let cleaned: Vec<Value> = raw
        .into_iter()
        // 跳过没有 role 的无效条目
        .filter(|m| m.get("role").and_then(|r| r.as_str()).is_some())
        .map(|mut m| {
            // content: null/缺失 → ""
            if m.get("content").map_or(true, |c| c.is_null()) {
                m["content"] = Value::String(String::new());
            }
            // assistant 消息必须携带 reasoning_content
            if m.get("role") == Some(&Value::String("assistant".into())) {
                if m.get("reasoning_content").map_or(true, |r| r.is_null()) {
                    m["reasoning_content"] = Value::String(String::new());
                }
            }
            // 空 tool_calls 数组 → 移除该字段
            if let Some(tc) = m.get("tool_calls") {
                if tc.as_array().map_or(true, |a| a.is_empty()) {
                    if let Some(obj) = m.as_object_mut() {
                        obj.remove("tool_calls");
                    }
                }
            }
            m
        })
        .collect();

    // 反序列化为 Message 结构（过滤无法解析的条目）
    cleaned
        .into_iter()
        .filter_map(|m| serde_json::from_value(m).ok())
        .collect()
}

/// 从 context.json 反序列化的原始消息中清理孤立 tool_call / tool_result
///
/// 规则（与 silences-llm 的 build_api_messages 一致）：
/// - 孤立 tool_call（无对应 tool_result）且 content 为空 → 移除
/// - 孤立 tool_result（无对应 assistant 的 tool_call） → 移除
pub fn remove_orphan_tool_messages(messages: &mut Vec<Message>) {
    // 收集有对应 tool_result 的 tool_call_id（clone 以避免借用冲突）
    let completed_ids: HashSet<String> = messages
        .iter()
        .filter(|m| m.role == "tool")
        .filter_map(|m| m.tool_call_id.clone())
        .collect();

    // 收集 assistant 声明的 tool_call_id
    let declared_ids: HashSet<String> = messages
        .iter()
        .filter(|m| m.role == "assistant")
        .filter_map(|m| m.tool_calls.as_ref())
        .flat_map(|tc| tc.iter().map(|t| t.id.clone()))
        .collect();

    // 重新借用：因为 completed_ids/declared_ids 是 owned String，不再借用 messages
    let completed_ids = &completed_ids;
    let declared_ids = &declared_ids;

    messages.retain(|m| {
        match m.role.as_str() {
            "assistant" => {
                // tool_calls 全孤立且 content 为空 → 移除
                if let Some(tc) = &m.tool_calls {
                    let all_orphan = tc.iter().all(|t| !completed_ids.contains(&t.id));
                    if all_orphan && m.content.is_empty() {
                        return false;
                    }
                }
                true
            }
            "tool" => {
                // 孤立 tool_result → 移除
                if let Some(ref id) = m.tool_call_id {
                    if !declared_ids.contains(id) {
                        return false;
                    }
                }
                true
            }
            _ => true,
        }
    });
}

#[cfg(test)]
mod tests {
    use super::*;
    use silences_core::{ToolCallValue, ToolCallFunction};

    // ── normalize_messages ──

    #[test]
    fn test_normalize_fixes_null_content() {
        let raw = vec![
            serde_json::json!({"role": "user", "content": null}),
            serde_json::json!({"role": "assistant", "content": "hi", "reasoning_content": null}),
        ];
        let msgs = normalize_messages(raw);
        assert_eq!(msgs.len(), 2);
        assert_eq!(msgs[0].content, "");
        assert_eq!(msgs[1].reasoning_content, Some("".into()));
    }

    #[test]
    fn test_normalize_removes_empty_tool_calls() {
        let raw = vec![
            serde_json::json!({"role": "assistant", "content": "ok", "tool_calls": []}),
        ];
        let msgs = normalize_messages(raw);
        assert_eq!(msgs.len(), 1);
        assert!(msgs[0].tool_calls.is_none());
    }

    #[test]
    fn test_normalize_keeps_valid_tool_calls() {
        let raw = vec![
            serde_json::json!({
                "role": "assistant",
                "content": "",
                "tool_calls": [{"id": "c1", "type": "function", "function": {"name": "read", "arguments": "{}"}}]
            }),
        ];
        let msgs = normalize_messages(raw);
        assert_eq!(msgs.len(), 1);
        assert!(msgs[0].tool_calls.is_some());
    }

    #[test]
    fn test_normalize_skips_no_role() {
        let raw = vec![
            serde_json::json!({"content": "no role here"}),
        ];
        let msgs = normalize_messages(raw);
        assert!(msgs.is_empty());
    }

    #[test]
    fn test_normalize_fills_missing_reasoning_for_assistant() {
        let raw = vec![
            serde_json::json!({"role": "assistant", "content": "hello"}),
        ];
        let msgs = normalize_messages(raw);
        assert_eq!(msgs[0].reasoning_content, Some("".into()));
    }

    #[test]
    fn test_normalize_does_not_change_user_reasoning() {
        let raw = vec![
            serde_json::json!({"role": "user", "content": "hello"}),
        ];
        let msgs = normalize_messages(raw);
        assert!(msgs[0].reasoning_content.is_none());
    }

    // ── remove_orphan_tool_messages ──

    #[test]
    fn test_remove_orphan_tool_call_with_empty_content() {
        let tc = ToolCallValue {
            id: "call_1".into(), call_type: "function".into(),
            function: ToolCallFunction { name: "read".into(), arguments: "{}".into() },
        };
        let mut msgs = vec![
            Message::new_tool_call(vec![tc]),
            // 没有对应的 tool_result
        ];
        remove_orphan_tool_messages(&mut msgs);
        assert!(msgs.is_empty());
    }

    #[test]
    fn test_remove_orphan_tool_result() {
        let mut msgs = vec![
            Message::new_tool_result("call_nonexistent", "result"),
        ];
        remove_orphan_tool_messages(&mut msgs);
        assert!(msgs.is_empty());
    }

    #[test]
    fn test_keep_valid_pair() {
        let tc = ToolCallValue {
            id: "call_1".into(), call_type: "function".into(),
            function: ToolCallFunction { name: "read".into(), arguments: "{}".into() },
        };
        let mut msgs = vec![
            Message::new_tool_call(vec![tc]),
            Message::new_tool_result("call_1", "content"),
        ];
        remove_orphan_tool_messages(&mut msgs);
        assert_eq!(msgs.len(), 2);
    }

    #[test]
    fn test_keep_orphan_with_nonempty_content() {
        let tc = ToolCallValue {
            id: "call_1".into(), call_type: "function".into(),
            function: ToolCallFunction { name: "read".into(), arguments: "{}".into() },
        };
        let mut asst = Message::new_tool_call(vec![tc]);
        asst.content = "思考结果".into();
        let mut msgs = vec![asst];
        remove_orphan_tool_messages(&mut msgs);
        assert_eq!(msgs.len(), 1);
        assert_eq!(msgs[0].content, "思考结果");
    }
}
