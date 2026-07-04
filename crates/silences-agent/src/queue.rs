//! 任务队列
//!
//! 线程安全。被 add_task 工具写入，start_task 按 ID 取出。
//! 内部用 HashMap 存储（O(1) 查找/删除），Vec 保持 FIFO 顺序。

use std::collections::HashMap;
use std::sync::Mutex;

use silences_core::TaskItem;

/// 线程安全的任务队列
pub struct TaskQueue {
    tasks: Mutex<HashMap<String, TaskItem>>,
    /// FIFO 顺序
    order: Mutex<Vec<String>>,
}

impl TaskQueue {
    pub fn new() -> Self {
        Self {
            tasks: Mutex::new(HashMap::new()),
            order: Mutex::new(Vec::new()),
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
}
