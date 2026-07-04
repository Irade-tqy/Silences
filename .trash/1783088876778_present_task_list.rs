//! present_task_list — 向用户展示任务列表并等待审批

use std::time::{SystemTime, UNIX_EPOCH};

use super::{ToolDef, ToolOutcome};

fn gen_approval_id() -> String {
    let ts = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_nanos();
    format!("ap_{:x}", ts)
}

pub fn tool() -> ToolDef {
    ToolDef {
        name: "present_task_list",
        description: "向用户展示拆分后的任务列表并等待审批。\nwhy: 需要列出任务或 task list 时使用。\nhow: 将完整的任务列表作为参数传入，系统会展示给用户审批。",
        schema: serde_json::json!({
            "type": "object",
            "properties": {
                "tasks": {
                    "type": "array",
                    "description": "任务列表",
                    "items": {
                        "type": "object",
                        "properties": {
                            "id": { "type": "string", "description": "任务 ID" },
                            "description": { "type": "string", "description": "任务描述" }
                        },
                        "required": ["id", "description"],
                        "additionalProperties": false
                    }
                }
            },
            "required": ["tasks"],
            "additionalProperties": false
        }),
        handler: Box::new(|args| Box::pin(async move {
            let tasks_str = serde_json::to_string_pretty(&args["tasks"])
                .unwrap_or_else(|_| "[]".to_string());
            eprintln!("[present_task_list] 等待审批:\n{}", tasks_str);
            Ok(ToolOutcome {
                summary: tasks_str.clone(),
                inverse: None,
                rollback: false,
                approval_pending: Some(gen_approval_id()),
                inject_messages: vec![],
                defer_rollback: false,
            })
        })),
    }
}
