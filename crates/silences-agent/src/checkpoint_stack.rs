//! 检查点栈
//!
//! 栈语义：push 添加检查点，rollback_to 回滚到目标（保留目标，清除之后的）。
//! 线程安全。被 checkpoint 工具写入，rollback 工具消费（清除后续检查点）。
//! 检查点列表通过 rollback 的 tool result 返回。
//!
//! 自动检查点（来自 usr msg）额外记录消息位置索引，供 rollback 截断消息上下文。

use std::collections::HashMap;
use std::sync::Mutex;

use silences_core::{CheckpointItem, Message};

/// 线程安全的检查点栈
pub struct CheckpointStack {
    items: Mutex<Vec<CheckpointItem>>,
    /// 自动检查点的消息位置索引：cp_id → 创建时的消息数量（即用户消息的后一个位置）
    auto_msg_indices: Mutex<HashMap<String, usize>>,
}

impl CheckpointStack {
    pub fn new() -> Self {
        Self {
            items: Mutex::new(Vec::new()),
            auto_msg_indices: Mutex::new(HashMap::new()),
        }
    }

    /// 清空
    pub fn clear(&self) {
        self.items.lock().unwrap().clear();
        self.auto_msg_indices.lock().unwrap().clear();
    }

    /// 入栈（用户创建的检查点）
    pub fn push(&self, id: String, description: String) {
        self.items.lock().unwrap().push(CheckpointItem { id, description, is_auto: false });
    }

    /// 入栈（自动检查点，来自 usr msg）
    /// `msg_count` 是创建时消息总数（含刚添加的用户消息），用于回滚时截断
    pub fn push_auto(&self, id: String, description: String, msg_count: usize) {
        self.items.lock().unwrap().push(CheckpointItem {
            id: id.clone(),
            description,
            is_auto: true,
        });
        self.auto_msg_indices.lock().unwrap().insert(id, msg_count);
    }

    /// 获取自动检查点的消息位置索引
    pub fn get_auto_msg_index(&self, id: &str) -> Option<usize> {
        self.auto_msg_indices.lock().unwrap().get(id).copied()
    }

    /// 检查点数量
    pub fn len(&self) -> usize {
        self.items.lock().unwrap().len()
    }

    /// 是否为空
    pub fn is_empty(&self) -> bool {
        self.items.lock().unwrap().is_empty()
    }

    /// 列表（顺序不变）
    pub fn list(&self) -> Vec<CheckpointItem> {
        self.items.lock().unwrap().clone()
    }

    /// 回滚到指定检查点：保留该检查点及之前的，清除之后的
    /// 如果 id 不存在则报错
    pub fn rollback_to(&self, id: &str) -> Result<(), String> {
        let mut items = self.items.lock().unwrap();
        let pos = items.iter().position(|c| c.id == id);
        match pos {
            Some(p) => {
                // 清理被移除检查点的 auto_msg_indices
                let removed: Vec<String> = items[(p + 1)..].iter().map(|c| c.id.clone()).collect();
                let mut indices = self.auto_msg_indices.lock().unwrap();
                for r in &removed {
                    indices.remove(r);
                }
                items.truncate(p + 1); // 保留目标及之前
                Ok(())
            }
            None => Err(format!("检查点 \"{id}\" 不存在")),
        }
    }

    /// 从消息历史重建栈（扫描 checkpoint/rollback tool_calls）
    pub fn rebuild_from_messages(messages: &[Message]) -> Self {
        let stack = Self::new();
        for msg in messages {
            if let Some(tcs) = &msg.tool_calls {
                for tc in tcs {
                    match tc.function.name.as_str() {
                        "checkpoint" => {
                            if let Ok(args) =
                                serde_json::from_str::<serde_json::Value>(&tc.function.arguments)
                            {
                                let id = args
                                    .get("id")
                                    .and_then(|v| v.as_str())
                                    .unwrap_or("?")
                                    .to_string();
                                let desc = args
                                    .get("description")
                                    .and_then(|v| v.as_str())
                                    .unwrap_or("?")
                                    .to_string();
                                stack.push(id, desc);
                            }
                        }
                        "rollback" => {
                            if let Ok(args) =
                                serde_json::from_str::<serde_json::Value>(&tc.function.arguments)
                            {
                                let id = args
                                    .get("checkpoint_id")
                                    .and_then(|v| v.as_str())
                                    .unwrap_or("");
                                let _ = stack.rollback_to(id);
                            }
                        }
                        _ => {}
                    }
                }
            }
        }
        stack
    }

    /// 格式化为 Markdown 列表（供 tool result 嵌入）
    pub fn format_for_context(&self) -> String {
        let items = self.items.lock().unwrap();
        if items.is_empty() {
            "_暂无检查点_".to_string()
        } else {
            let mut parts = Vec::new();
            for (i, cp) in items.iter().enumerate() {
                let auto_tag = if cp.is_auto { " [自动]" } else { "" };
                let sep = if cp.description.is_empty() { "" } else { ": " };
                let hint = if cp.is_auto {
                    format!("\n   -> rollback 时使用 `rollback(checkpoint_id=\"{}\")`", cp.id)
                } else {
                    String::new()
                };
                parts.push(format!("{}. **{}**{}{}{}{}",
                    i + 1, cp.id, auto_tag, sep, cp.description, hint));
            }
            parts.join("\n")
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_new_stack_empty() {
        let s = CheckpointStack::new();
        assert!(s.is_empty());
        assert_eq!(s.len(), 0);
    }

    #[test]
    fn test_push_and_list() {
        let s = CheckpointStack::new();
        s.push("c1".into(), "first".into());
        s.push("c2".into(), "second".into());
        let list = s.list();
        assert_eq!(list.len(), 2);
        assert_eq!(list[0].id, "c1");
        assert_eq!(list[1].id, "c2");
    }

    #[test]
    fn test_rollback_to_keeps_target() {
        let s = CheckpointStack::new();
        s.push("c1".into(), "first".into());
        s.push("c2".into(), "second".into());
        s.push("c3".into(), "third".into());

        assert!(s.rollback_to("c2").is_ok());
        let list = s.list();
        assert_eq!(list.len(), 2);
        assert_eq!(list[0].id, "c1");
        assert_eq!(list[1].id, "c2");
    }

    #[test]
    fn test_rollback_to_first() {
        let s = CheckpointStack::new();
        s.push("c1".into(), "a".into());
        s.push("c2".into(), "b".into());
        assert!(s.rollback_to("c1").is_ok());
        assert_eq!(s.len(), 1);
        assert_eq!(s.list()[0].id, "c1");
    }

    #[test]
    fn test_rollback_to_non_existent_errors() {
        let s = CheckpointStack::new();
        s.push("c1".into(), "a".into());
        let r = s.rollback_to("c_nonexist");
        assert!(r.is_err());
        assert_eq!(s.len(), 1); // 状态不变
    }

    #[test]
    fn test_rollback_to_empty_stack_errors() {
        let s = CheckpointStack::new();
        assert!(s.rollback_to("c1").is_err());
    }

    #[test]
    fn test_format_for_context_empty() {
        let s = CheckpointStack::new();
        let fmt = s.format_for_context();
        assert_eq!(fmt, "_暂无检查点_");
    }

    #[test]
    fn test_format_for_context_non_empty() {
        let s = CheckpointStack::new();
        s.push("c1".into(), "处理 A".into());
        s.push("c2".into(), "处理 B".into());
        let fmt = s.format_for_context();
        assert!(fmt.contains("c1"));
        assert!(fmt.contains("c2"));
        assert!(fmt.contains("处理 A"));
    }

    #[test]
    fn test_clear() {
        let s = CheckpointStack::new();
        s.push("c1".into(), "a".into());
        s.push("c2".into(), "b".into());
        s.clear();
        assert!(s.is_empty());
    }

    #[test]
    fn test_push_after_rollback() {
        let s = CheckpointStack::new();
        s.push("c1".into(), "a".into());
        s.push("c2".into(), "b".into());
        s.rollback_to("c1").unwrap();
        s.push("c3".into(), "c".into());
        let list = s.list();
        assert_eq!(list.len(), 2);
        assert_eq!(list[0].id, "c1");
        assert_eq!(list[1].id, "c3");
    }

    #[test]
    fn test_push_auto() {
        let s = CheckpointStack::new();
        s.push_auto("cp_auto1".into(), "用户消息".into(), 5);
        let list = s.list();
        assert_eq!(list.len(), 1);
        assert!(list[0].is_auto);
        assert_eq!(s.get_auto_msg_index("cp_auto1"), Some(5));
    }

    #[test]
    fn test_rollback_to_cleans_auto_indices() {
        let s = CheckpointStack::new();
        s.push_auto("cp_a".into(), "msg1".into(), 5);
        s.push_auto("cp_b".into(), "msg2".into(), 10);
        s.push_auto("cp_c".into(), "msg3".into(), 15);

        assert!(s.rollback_to("cp_b").is_ok());
        assert_eq!(s.get_auto_msg_index("cp_a"), Some(5));
        assert_eq!(s.get_auto_msg_index("cp_b"), Some(10));
        assert_eq!(s.get_auto_msg_index("cp_c"), None);
        assert_eq!(s.list().len(), 2);
    }

    #[test]
    fn test_format_for_context_with_auto() {
        let s = CheckpointStack::new();
        s.push("task1".into(), "处理 A".into());
        s.push_auto("cp_auto1".into(), "用户消息".into(), 5);
        let fmt = s.format_for_context();
        assert!(fmt.contains("task1"));
        assert!(fmt.contains("cp_auto1"));
        assert!(fmt.contains("[自动]"));
        assert!(fmt.contains("rollback(checkpoint_id"));
    }
}
