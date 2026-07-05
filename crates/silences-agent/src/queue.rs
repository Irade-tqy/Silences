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
        let item = self.remove(id);
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
