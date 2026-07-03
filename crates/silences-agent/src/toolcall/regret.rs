//! regret — 撤销上一个工具调用的结果

use std::collections::VecDeque;
use std::sync::Arc;

use anyhow::{Context, Result};
use tokio::sync::Mutex;

use super::{InverseOp, ToolDef, ToolOutcome};

/// 工具历史记录
pub struct ToolHistory {
    entries: VecDeque<(String, InverseOp)>,
    max_len: usize,
}

impl ToolHistory {
    pub fn new(max_len: usize) -> Self {
        Self {
            entries: VecDeque::new(),
            max_len,
        }
    }

    /// 记录一个工具操作
    pub fn push(&mut self, tool_name: &str, inverse: InverseOp) {
        if self.entries.len() >= self.max_len {
            self.entries.pop_front();
        }
        self.entries.push_back((tool_name.to_string(), inverse));
    }

    /// 撤销最近一次操作，返回撤销的摘要
    pub fn undo(&mut self) -> Result<String> {
        let (tool_name, inverse) = self
            .entries
            .pop_back()
            .context("没有可撤销的操作")?;

        let result = inverse.apply()?;
        Ok(format!("[UNDO] {tool_name}: {result}"))
    }

    /// 检查是否有可撤销的操作
    pub fn can_undo(&self) -> bool {
        !self.entries.is_empty()
    }
}

// ===== 工具定义 =====

pub fn tool(history: Arc<Mutex<ToolHistory>>) -> ToolDef {
    ToolDef {
        name: "regret",
        description:
            "撤销上一个工具调用的结果。\nwhy: 当操作结果不符合预期时调用。\nhow: 最多连续撤销 5 次。不支持撤销 command 的结果。",
        schema: serde_json::json!({
            "type": "object",
            "properties": {},
            "required": [],
            "additionalProperties": false
        }),
        handler: Box::new(move |_args| {
            let h = history.clone();
            Box::pin(async move {
                let mut history = h.lock().await;
                let summary = history.undo().unwrap_or_else(|e| format!("regret 失败: {e}"));
                Ok(ToolOutcome {
                    summary,
                    inverse: None,
                
        rollback: false,
                
        approval_pending: None,
                })
            })
        }),
    }
}
