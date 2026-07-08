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
        description: "在当前位置打一个检查点。每个 checkpoint 必须配合后续 rollback 使用。\nwhy: 开始一个独立子任务时创建安全回滚点，修改完成后必须 rollback 清空上下文再继续下一个任务。\nhow: id 用简短有意义的英文/数字（如 \"task_fix_login\"），传入 rollback(checkpoint_id=...) 时使用。修改完成后调用 rollback。",
        schema: serde_json::json!({
            "type": "object",
            "properties": {
                "id": {
                    "type": "string",
                    "description": "检查点 ID（简短、可读，如 \"task_fix_login\"，后续 rollback 时使用此值）"
                },
                "description": {
                    "type": "string",
                    "description": "简短的检查点描述，方便辨认（会显示在 list_checkpoints 输出中）"
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
