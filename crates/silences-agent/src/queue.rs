//! 任务队列
//!
//! 线程安全。被 add_task 工具写入，start_task 按 ID 取出。
//! 内部用 HashMap 存储（O(1) 查找/删除），Vec 保持 FIFO 顺序。
//! 已完成任务单独记录，供上下文注入任务列表。

use std::collections::HashMap;
use std::sync::Mutex;

use silences_core::TaskItem;

/// 线程安全的任务队列
pub struct TaskQueue {
    tasks: Mutex<HashMap<String, TaskItem>>,
    /// FIFO 顺序
    order: Mutex<Vec<String>>,
    /// 已完成任务（按完成顺序）
    completed: Mutex<Vec<TaskItem>>,
    /// 当前活跃任务（start_task 设置，end_task 清除）
    active: Mutex<Option<TaskItem>>,
}

impl TaskQueue {
    pub fn new() -> Self {
        Self {
            tasks: Mutex::new(HashMap::new()),
            order: Mutex::new(Vec::new()),
            completed: Mutex::new(Vec::new()),
            active: Mutex::new(None),
        }
    }

    /// 添加任务到队列尾部
    pub fn add(&self, id: String, description: String) {
        let mut tasks = self.tasks.lock().unwrap();
        let mut order = self.order.lock().unwrap();
        let item = TaskItem { id: id.clone(), description };
        tasks.insert(id.clone(), item);
        order.push(id);
    }

    /// 按 ID 移除任务并返回，None 表示不存在
    pub fn remove(&self, id: &str) -> Option<TaskItem> {
        let mut tasks = self.tasks.lock().unwrap();
        let mut order = self.order.lock().unwrap();
        order.retain(|i| i != id);
        tasks.remove(id)
    }

    /// 从队列头部弹出一个任务（FIFO）
    pub fn pop_front(&self) -> Option<TaskItem> {
        let mut tasks = self.tasks.lock().unwrap();
        let mut order = self.order.lock().unwrap();
        let id = order.first()?.clone();
        order.remove(0);
        tasks.remove(&id)
    }

    /// 队列是否为空
    pub fn is_empty(&self) -> bool {
        let tasks = self.tasks.lock().unwrap();
        tasks.is_empty()
    }

    /// 返回当前队列中所有任务（FIFO 顺序）
    pub fn list(&self) -> Vec<TaskItem> {
        let order = self.order.lock().unwrap();
        let tasks = self.tasks.lock().unwrap();
        order.iter().filter_map(|id| tasks.get(id).cloned()).collect()
    }

    /// 设置当前活跃任务（由 start_task 调用）
    pub fn set_active(&self, id: &str, description: &str) {
        *self.active.lock().unwrap() = Some(TaskItem {
            id: id.to_string(),
            description: description.to_string(),
        });
    }

    /// 清除当前活跃任务（由 end_task 调用）
    pub fn clear_active(&self) {
        *self.active.lock().unwrap() = None;
    }

    /// 是否有活跃任务（add + start 后还没 end）
    pub fn has_active(&self) -> bool {
        self.active.lock().unwrap().is_some()
    }

    /// 标记任务已完成：从待处理移到已完成列表末尾，清除活跃状态
    pub fn complete_task(&self, id: &str) {
        // start_task 已从 tasks 中移除，但设了 active
        // 先尝试从 tasks 中移除，若不在则从 active 取
        let item = self.remove(id).or_else(|| {
            self.active.lock().unwrap().take()
        });
        if let Some(item) = item {
            *self.active.lock().unwrap() = None;
            let mut completed = self.completed.lock().unwrap();
            completed.push(item);
        }
    }

    /// 返回已完成任务列表
    pub fn completed_list(&self) -> Vec<TaskItem> {
        let completed = self.completed.lock().unwrap();
        completed.clone()
    }

    /// 生成供上下文注入的任务列表 Markdown（已完成 + 待处理）
    pub fn format_for_context(&self) -> String {
        let completed = self.completed.lock().unwrap();
        let order = self.order.lock().unwrap();
        let tasks = self.tasks.lock().unwrap();

        let mut parts = Vec::new();

        // 已完成
        if !completed.is_empty() {
            parts.push("### 已完成".to_string());
            for t in completed.iter() {
                parts.push(format!("- {}: {}", t.id, t.description));
            }
        }

        // 待处理
        if !order.is_empty() {
            parts.push("### 待处理".to_string());
            for id in order.iter() {
                if let Some(t) = tasks.get(id) {
                    parts.push(format!("- {}: {}", t.id, t.description));
                }
            }
        } else if completed.is_empty() {
            parts.push("_暂无任务_".to_string());
        }

        parts.join("\n")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_new_queue_empty() {
        let queue = TaskQueue::new();
        assert!(queue.is_empty());
        assert!(queue.list().is_empty());
    }

    #[test]
    fn test_add_and_list() {
        let queue = TaskQueue::new();
        queue.add("t1".into(), "Task 1".into());
        queue.add("t2".into(), "Task 2".into());

        let tasks = queue.list();
        assert_eq!(tasks.len(), 2);
        assert_eq!(tasks[0].id, "t1");
        assert_eq!(tasks[1].id, "t2");
    }

    #[test]
    fn test_pop_front_fifo() {
        let queue = TaskQueue::new();
        queue.add("a".into(), "A".into());
        queue.add("b".into(), "B".into());
        queue.add("c".into(), "C".into());

        assert_eq!(queue.pop_front().unwrap().id, "a");
        assert_eq!(queue.pop_front().unwrap().id, "b");
        assert_eq!(queue.pop_front().unwrap().id, "c");
        assert!(queue.pop_front().is_none());
    }

    #[test]
    fn test_remove_by_id() {
        let queue = TaskQueue::new();
        queue.add("keep".into(), "Keep".into());
        queue.add("remove_me".into(), "Remove".into());

        let removed = queue.remove("remove_me");
        assert!(removed.is_some());
        assert_eq!(removed.unwrap().id, "remove_me");

        let tasks = queue.list();
        assert_eq!(tasks.len(), 1);
        assert_eq!(tasks[0].id, "keep");
    }

    #[test]
    fn test_remove_non_existent() {
        let queue = TaskQueue::new();
        queue.add("t1".into(), "T1".into());
        assert!(queue.remove("non_existent").is_none());
        assert_eq!(queue.list().len(), 1);
    }

    #[test]
    fn test_is_empty() {
        let queue = TaskQueue::new();
        assert!(queue.is_empty());

        queue.add("t".into(), "T".into());
        assert!(!queue.is_empty());

        queue.pop_front();
        assert!(queue.is_empty());
    }

    #[test]
    fn test_complete_task() {
        let queue = TaskQueue::new();
        queue.add("a".into(), "A".into());
        queue.add("b".into(), "B".into());

        queue.set_active("a", "A");
        queue.complete_task("a");

        assert!(queue.completed_list().len() == 1);
        assert_eq!(queue.completed_list()[0].id, "a");
        assert_eq!(queue.list().len(), 1);
        assert_eq!(queue.list()[0].id, "b");
    }

    #[test]
    fn test_has_active() {
        let queue = TaskQueue::new();
        assert!(!queue.has_active());

        queue.set_active("t", "T");
        assert!(queue.has_active());

        queue.clear_active();
        assert!(!queue.has_active());
    }

    #[test]
    fn test_format_for_context_completed_and_pending() {
        let queue = TaskQueue::new();
        queue.add("t1".into(), "Task 1".into());
        queue.add("t2".into(), "Task 2".into());
        queue.complete_task("t1");

        let formatted = queue.format_for_context();
        assert!(formatted.contains("### 已完成"));
        assert!(formatted.contains("### 待处理"));
        assert!(formatted.contains("- t1: Task 1"));
        assert!(formatted.contains("- t2: Task 2"));
    }

    #[test]
    fn test_format_for_context_empty() {
        let queue = TaskQueue::new();
        let formatted = queue.format_for_context();
        assert_eq!(formatted, "_暂无任务_");
    }

    #[test]
    fn test_completed_list_order() {
        let queue = TaskQueue::new();
        queue.add("a".into(), "A".into());
        queue.add("b".into(), "B".into());
        queue.complete_task("a");
        queue.complete_task("b");

        let completed = queue.completed_list();
        assert_eq!(completed.len(), 2);
        assert_eq!(completed[0].id, "a");
        assert_eq!(completed[1].id, "b");
    }
}
