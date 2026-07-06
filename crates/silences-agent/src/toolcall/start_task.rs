//! start_task 工具：从队列中取出并开始一个指定的任务
//!
//! 按 task_id 从队列中移除任务，如果队列中不存在该 ID 则报错。
//! 调用后模型应立即开始执行此任务。

use std::sync::Arc;

use super::{ToolDef, ToolOutcome};
use crate::queue::TaskQueue;

pub fn tool(queue: Arc<TaskQueue>) -> ToolDef {
    ToolDef {
        name: "start_task",
        description: "开始执行指定任务[不可撤销]",
        schema: serde_json::json!({
            "type": "object",
            "properties": {
                "task_id": {
                    "type": "string",
                    "description": "已 add 的任务 ID"
                },
                "description": {
                    "type": "string",
                    "description": "当前任务描述"
                }
            },
            "required": ["task_id"],
            "additionalProperties": false
        }),
        handler: Box::new(move |args| {
            let q = Arc::clone(&queue);
            Box::pin(async move {
                let task_id = args["task_id"].as_str().unwrap_or("unknown").to_string();
                let desc = args["description"].as_str().unwrap_or("");

                let removed = q.remove(&task_id);
                let queue_status = if q.is_empty() { "队列已空" } else { "队列中还有任务" };

                if removed.is_some() {
                    q.set_active(&task_id, desc);
                    let summary = format!("[开始任务] {}: {} ({})", task_id, desc, queue_status);
                    Ok(ToolOutcome::new(summary))
                } else {
                    // 队列中不存在该 ID —— 可能已完成或从未添加
                    let err = format!("[start_task] 错误: 队列中不存在任务 \"{task_id}\"。可能已被移除或从未添加。可用 add_task 先添加。");
                    // 不 eprintln 此错误，因为 summary 已包含错误信息
                    // 返回普通结果，不是 Err，这样 LLM 可以恢复
                    Ok(ToolOutcome::new(err))
                }
            })
        }),
    }
}
