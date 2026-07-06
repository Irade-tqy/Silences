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
            "撤销上一个工具的结果。\nwhy: 操作不符合预期时回退[不可撤销]",
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
                Ok(ToolOutcome::new(summary))
            })
        }),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_new_history_empty() {
        let mut history = ToolHistory::new(5);
        assert!(!history.can_undo());
        assert!(history.undo().is_err());
    }

    #[test]
    fn test_push_and_undo_lifo() {
        let mut history = ToolHistory::new(10);
        history.push("read", InverseOp::new("undo read".into(), || Ok("done".into())));
        history.push("edit", InverseOp::new("undo edit".into(), || Ok("done".into())));

        assert!(history.can_undo());
        assert_eq!(history.undo().unwrap(), "[UNDO] edit: done");
        assert_eq!(history.undo().unwrap(), "[UNDO] read: done");
        assert!(!history.can_undo());
    }

    #[test]
    fn test_push_respects_max_size() {
        let mut history = ToolHistory::new(2);
        history.push("a", InverseOp::new("a".into(), || Ok("a_out".into())));
        history.push("b", InverseOp::new("b".into(), || Ok("b_out".into())));
        history.push("c", InverseOp::new("c".into(), || Ok("c_out".into())));

        // "a" was evicted when "c" was pushed
        assert_eq!(history.undo().unwrap(), "[UNDO] c: c_out");
        assert_eq!(history.undo().unwrap(), "[UNDO] b: b_out");
        assert!(history.undo().is_err());
    }

    #[test]
    fn test_can_undo_reflects_state() {
        let mut history = ToolHistory::new(3);
        assert!(!history.can_undo());

        history.push("x", InverseOp::new("x".into(), || Ok("".into())));
        assert!(history.can_undo());

        history.undo().unwrap();
        assert!(!history.can_undo());
    }

    #[test]
    fn test_undo_on_empty_error_message() {
        let mut history = ToolHistory::new(3);
        let err = history.undo().unwrap_err();
        assert!(err.to_string().contains("没有可撤销的操作"));
    }
}
