//! 检查点栈
//!
//! 栈语义：push 添加检查点，rollback_to 回滚到目标（保留目标，清除之后的）。
//! 线程安全。被 checkpoint 工具写入，rollback 工具消费（清除后续检查点）。
//! 检查点列表通过 rollback 的 tool result 返回。

use std::sync::Mutex;

use silences_core::{CheckpointItem, Message};

/// 线程安全的检查点栈
pub struct CheckpointStack {
    items: Mutex<Vec<CheckpointItem>>,
}

impl CheckpointStack {
    pub fn new() -> Self {
        Self { items: Mutex::new(Vec::new()) }
    }

    /// 清空
    pub fn clear(&self) {
        self.items.lock().unwrap().clear();
    }

    /// 入栈
    pub fn push(&self, id: String, description: String) {
        self.items.lock().unwrap().push(CheckpointItem { id, description });
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
                            if let Ok(args) = serde_json::from_str::<serde_json::Value>(&tc.function.arguments) {
                                let id = args.get("id").and_then(|v| v.as_str()).unwrap_or("?").to_string();
                                let desc = args.get("description").and_then(|v| v.as_str()).unwrap_or("?").to_string();
                                stack.push(id, desc);
                            }
                        }
                        "rollback" => {
                            if let Ok(args) = serde_json::from_str::<serde_json::Value>(&tc.function.arguments) {
                                let id = args.get("checkpoint_id").and_then(|v| v.as_str()).unwrap_or("");
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
                parts.push(format!("{}. **{}**: {}", i + 1, cp.id, cp.description));
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
}
