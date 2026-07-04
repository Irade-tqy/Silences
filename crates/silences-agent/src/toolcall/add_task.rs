//! add_task — 向任务队列添加新任务
//!
//! 无审批，模型可以自由添加任务。任务按 FIFO 顺序执行，
//! 但 start_task 可按 ID 选取任意任务以任意顺序开工。

use std::sync::Arc;

use super::{ToolDef, ToolOutcome};
use crate::queue::TaskQueue;

pub fn tool(queue: Arc<TaskQueue>) -> ToolDef {
    ToolDef {
        name: "add_task",
        description: "向动态任务队列添加一个新任务。\nwhy: 发现需要做的事情但当前不紧急时，先入队稍后自动处理。\nhow: 提供任务 id（唯一标识）和描述。",
        schema: serde_json::json!({
            "type": "object",
            "properties": {
                "id": {
                    "type": "string",
                    "description": "任务 ID，应唯一描述此项任务"
                },
                "description": {
                    "type": "string",
                    "description": "任务描述，说明要做什么"
                }
            },
            "required": ["id", "description"],
            "additionalProperties": false
        }),
        handler: Box::new(move |args| {
            let q = Arc::clone(&queue);
            Box::pin(async move {
                let id = args["id"].as_str().unwrap_or("unknown").to_string();
                let description = args["description"].as_str().unwrap_or("").to_string();

                q.add(id.clone(), description.clone());

                let summary = format!("[添加任务] {}: {}", id, description);
                eprintln!("{summary}");
                Ok(ToolOutcome {
                    summary,
                    inverse: None,
                    rollback: false,
                    approval_pending: None,
                    inject_messages: vec![],
                    defer_rollback: false,
                })
            })
        }),
    }
}
