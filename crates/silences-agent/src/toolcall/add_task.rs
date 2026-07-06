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
        description: "添加一个新任务[不可撤销]",
        schema: serde_json::json!({
            "type": "object",
            "properties": {
                "id": {
                    "type": "string",
                    "description": "任务 ID，应唯一"
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
                Ok(ToolOutcome::new(summary))
            })
        }),
    }
}
