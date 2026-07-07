//! list_checkpoints — 列出当前所有检查点
//!
//! 展示检查点栈中所有已创建的检查点，按添加顺序排列。

use std::sync::Arc;

use super::{ToolDef, ToolOutcome};
use crate::checkpoint_stack::CheckpointStack;

pub fn tool(stack: Arc<CheckpointStack>) -> ToolDef {
    ToolDef {
        name: "list_checkpoints",
        description: "列出当前所有检查点\nwhy: 查看可用的检查点列表，选择回滚目标\n注意：输出中每个检查点的 ID（加粗文本）可直接用于 rollback(checkpoint_id=...) 的第一个参数。",
        schema: serde_json::json!({
            "type": "object",
            "properties": {},
            "required": [],
            "additionalProperties": false
        }),
        handler: Box::new(move |_args| {
            let s = Arc::clone(&stack);
            Box::pin(async move {
                let formatted = s.format_for_context();
                let summary = format!("📋 当前检查点（可在 rollback 中使用加粗的 ID）：\n{formatted}");
                Ok(ToolOutcome::new(summary))
            })
        }),
    }
}
