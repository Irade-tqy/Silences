//! checkpoint — 在当前位置打一个检查点
//!
//! 后续可以通过 rollback 回滚到这里。检查点按打桩顺序入栈。
//! 现在由 LLM 根据系统提示词主动调用（用户可在 DB system prompt 中加入指令）。

use std::sync::Arc;

use super::{ToolDef, ToolOutcome};
use crate::checkpoint_stack::CheckpointStack;

pub fn tool(stack: Arc<CheckpointStack>) -> ToolDef {
    ToolDef {
        name: "checkpoint",
        description: "在当前位置打一个检查点\nwhy: 开始一个独立任务时",
        schema: serde_json::json!({
            "type": "object",
            "properties": {
                "id": {
                    "type": "string",
                    "description": "检查点 ID"
                },
                "description": {
                    "type": "string",
                    "description": "简短的检查点描述，方便辨认"
                }
            },
            "required": ["id", "description"],
            "additionalProperties": false
        }),
        handler: Box::new(move |args| {
            let s = Arc::clone(&stack);
            Box::pin(async move {
                let id = args["id"].as_str().unwrap_or("unknown").to_string();
                let description = args["description"].as_str().unwrap_or("").to_string();

                // 防重：ID 已存在则报错
                if s.list().iter().any(|c| c.id == id) {
                    return Ok(ToolOutcome::new(format!("❌ 检查点 \"{id}\" 已存在，请使用不同的 ID")));
                }

                s.push(id.clone(), description.clone());

                let summary = format!("✅ 已创建检查点 {}: {}", id, description);
                Ok(ToolOutcome::new(summary))
            })
        }),
    }
}
