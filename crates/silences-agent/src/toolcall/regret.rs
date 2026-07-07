//! regret — 撤销上一个工具调用的结果

use std::collections::VecDeque;
use std::sync::Arc;

use anyhow::{Context, Result};
use serde_json::Value;
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

    /// 返回条目引用（用于显示）
    pub fn entries(&self) -> &VecDeque<(String, InverseOp)> {
        &self.entries
    }
}

// ===== 工具定义 =====

pub fn tool(history: Arc<Mutex<ToolHistory>>) -> ToolDef {
    ToolDef {
        name: "regret",
        description:
            "撤销可逆操作（edit / block_edit / replace / write / trash / rename）。使用 show=true 查看可撤销操作队列。\n注意：只读/command/任务管理/regret 自身等不可逆操作不会进队，无法撤销。可逆操作按后进先出 (LIFO) 顺序撤销。\nwhy: 操作不符合预期时回退[不可撤销]",
        schema: serde_json::json!({
            "type": "object",
            "properties": {
                "show": {
                    "type": "boolean",
                    "description": "true=仅显示可撤销操作列表，不执行撤销（默认 false）"
                }
            },
            "required": [],
            "additionalProperties": false
        }),
        handler: Box::new(move |args| {
            let h = history.clone();
            Box::pin(async move {
                let mut history = h.lock().await;
                let show = args.get("show").and_then(Value::as_bool).unwrap_or(false);
                if show {
                    if !history.can_undo() {
                        Ok(ToolOutcome::new("regret: 当前无可撤销的操作。"))
                    } else {
                        let lines: Vec<String> = history
                            .entries()
                            .iter()
                            .enumerate()
                            .rev()
                            .map(|(i, (tool_name, inv))| {
                                format!("{}. [{}] {}", i + 1, tool_name, inv.description)
                            })
                            .collect();
                        Ok(ToolOutcome::new(format!(
                            "regret: {} 条可撤销操作（最近操作在前）:\n{}",
                            lines.len(),
                            lines.join("\n"),
                        )))
                    }
                } else {
                    let summary = history.undo().unwrap_or_else(|e| format!("regret 失败: {e}"));
                    Ok(ToolOutcome::new(summary))
                }
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

    #[test]
    fn test_entries_empty() {
        let history = ToolHistory::new(5);
        assert!(history.entries().is_empty());
    }

    #[test]
    fn test_entries_returns_items() {
        let mut history = ToolHistory::new(5);
        history.push("edit", InverseOp::new("edit on foo.rs".into(), || Ok("ok".into())));
        history.push("write", InverseOp::new("write bar.py".into(), || Ok("ok".into())));
        let entries = history.entries();
        assert_eq!(entries.len(), 2);
        assert_eq!(entries[0].0, "edit");
        assert_eq!(entries[0].1.description, "edit on foo.rs");
        assert_eq!(entries[1].0, "write");
        assert_eq!(entries[1].1.description, "write bar.py");
    }

    #[test]
    fn test_entries_not_affected_by_undo() {
        let mut history = ToolHistory::new(5);
        history.push("edit", InverseOp::new("edit".into(), || Ok("ok".into())));
        history.push("write", InverseOp::new("write".into(), || Ok("ok".into())));
        history.undo().unwrap(); // removes "write"
        assert_eq!(history.entries().len(), 1);
        assert_eq!(history.entries()[0].0, "edit");
    }
}
