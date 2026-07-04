//! end_task 工具：标记任务完成，触发上下文回退
//!
//! 调用 end_task 后，agent 循环会：
//! 1. 将任务标记为已完成
//! 2. 注入 u_orch 消息，指示模型更新 CONTEXT.md
//! 3. 模型用 write/edit 更新 CONTEXT.md（记录任务完成进度）
//! 4. 回退消息到 checkpoint（砍掉本轮多余上下文）
//! 5. 下一轮读取最新的 CONTEXT.md 并注入任务列表继续执行

use std::sync::Arc;

use silences_core::Message;
use super::{ToolDef, ToolOutcome};
use crate::queue::TaskQueue;

pub fn tool(queue: Arc<TaskQueue>) -> ToolDef {
    ToolDef {
        name: "end_task",
        description: "完成当前任务并记录摘要。\nwhy: 标记工作完成，系统将自动更新任务列表并提示你更新 CONTEXT.md，然后回退上下文。如果队列中还有任务将继续执行，否则系统会请求最终总结。\nhow: 调用 end_task 后请等待系统指示更新 CONTEXT.md。",
        schema: serde_json::json!({
            "type": "object",
            "properties": {
                "task_id": {
                    "type": "string",
                    "description": "任务 ID"
                },
                "summary": {
                    "type": "string",
                    "description": "完成摘要，记录关键成果"
                }
            },
            "required": ["task_id", "summary"],
            "additionalProperties": false
        }),
        handler: Box::new(move |args| {
            let q = Arc::clone(&queue);
            Box::pin(async move {
                let task_id = args["task_id"].as_str().unwrap_or("unknown").to_string();
                let summary = args["summary"].as_str().unwrap_or("");
                // 标记为已完成（从待处理移到已完成列表）
                q.complete_task(&task_id);
                let msg = format!("[完成任务] {}: {}", task_id, summary);
                eprintln!("{msg}");
                Ok(ToolOutcome {
                    summary: msg,
                    inverse: None,
                    rollback: true,
                    approval_pending: None,
                    // 注入 u_orch 指示模型更新 CONTEXT.md
                    inject_messages: vec![Message::new_user("orch",
                        &format!("任务 {task_id} 已完成。只更新 CONTEXT.md，记录完成进度，输出一个对本轮已完成工作的简要总结，然后停下。")),
                    ],
                    defer_rollback: true,
                })
            })
        }),
    }
}
