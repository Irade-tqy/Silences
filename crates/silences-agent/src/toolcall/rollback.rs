//! rollback — 回滚到指定检查点
//!
//! 回滚后：
//! 1. 目标检查点保留，后续检查点自动清除
//! 2. 消息上下文截断到目标检查点（保留 checkpoint + rollback 工具记录）
//! 3. LLM 应更新 CONTEXT.md 记录进度并输出总结
//! 4. 当前检查点列表通过 tool result 告知模型

use std::sync::Arc;

use silences_core::Message;
use super::{ToolDef, ToolOutcome};
use crate::checkpoint_stack::CheckpointStack;

pub fn tool(stack: Arc<CheckpointStack>) -> ToolDef {
    ToolDef {
        name: "rollback",
        description: "回滚到指定检查点。回滚会自动更新 CONTEXT.md 并用总结覆盖过程。\nwhy: 放弃本轮后续操作，回到打检查点时的状态。\nhow: 先调 list_checkpoints 查看可用 ID，然后传入对应 ID。",
        schema: serde_json::json!({
            "type": "object",
            "properties": {
                "checkpoint_id": {
                    "type": "string",
                    "description": "目标检查点 ID（从 list_checkpoints 的输出中复制，例如 \"cp_1a2b3c4d\"）"
                }
            },
            "required": ["checkpoint_id"],
            "additionalProperties": false
        }),
        handler: Box::new(move |args| {
            let s = Arc::clone(&stack);
            Box::pin(async move {
                let cp_id = args["checkpoint_id"].as_str().unwrap_or("").to_string();

                // 栈中回滚：保留目标检查点，清除之后
                if let Err(e) = s.rollback_to(&cp_id) {
                    return Ok(ToolOutcome::new(format!("❌ 回滚失败：{e}")));
                }

                Ok(ToolOutcome {
                    // 阶段 1：简短确认，等模型更新 CONTEXT.md 后在 pending_rollback 中覆写
                    summary: "回滚中".into(),
                    inverse: None,
                    rollback: true,
                    approval_pending: None,
                    // 注入 user orch 指示模型更新 CONTEXT.md，放下当前任务
                    inject_messages: vec![
                        Message::new_user("orch",
                            &format!(
                                "已回滚到检查点 `{cp_id}`。请更新 CONTEXT.md 记录已完成的工作进度，确保反映当前状态。放下当前任务，然后输出对本轮工作的简要总结。"
                            )
                        ),
                    ],
                    defer_rollback: true,
                })
            })
        }),
    }
}
