//! 等待队列模块 —— 提供线程阻塞/唤醒机制，支持基于键的精确唤醒和带超时的睡眠。
//!
//! 等待队列是内核同步原语的基础组件，用于实现信号量、futex、epoll 等阻塞等待操作。

use std::sync::{Mutex, atomic::{AtomicUsize, Ordering}};
use std::collections::VecDeque;
use std::thread;
use std::time::Duration;

/// 等待队列结构体，管理阻塞线程的队列。
///
/// 每个条目包含一个键（用于精确匹配唤醒）、被阻塞线程的句柄和一个标志位。
/// 支持按 key 唤醒单个/所有等待者、带超时的睡眠以及基于谓词的过滤唤醒。
pub struct WaitQueue {
    /// 内部等待队列，每项为 `(key, thread, flags)`。
    pub inner: Mutex<VecDeque<(usize, thread::Thread, u32)>>,
    /// 累计唤醒次数计数器。
    pub wake_count: AtomicUsize,
}

impl WaitQueue {
    /// 创建一个新的空等待队列。
    pub fn new() -> Self {
        eprintln!("[DBG] WaitQueue::new");
        Self {
            inner: Mutex::new(VecDeque::new()),
            wake_count: AtomicUsize::new(0),
        }
    }

    /// 将当前线程以指定 key 和 flags 加入等待队列，并阻塞（park）当前线程。
    pub fn sleep(&self, key: usize, flags: u32) {
        eprintln!("[DBG] WaitQueue::sleep");
        let mut q = self.inner.lock().unwrap();
        q.push_back((key, thread::current(), flags));
        drop(q);
        thread::park();
    }

    /// 带超时时间的睡眠：在指定超时后自动唤醒。
    ///
    /// 返回 true 表示是超时唤醒（该条目仍被保留并被移除），false 表示被外部唤醒。
    pub fn sleep_timeout(&self, key: usize, flags: u32, timeout: Duration) -> bool {
        eprintln!("[DBG] WaitQueue::sleep_timeout");
        let mut q = self.inner.lock().unwrap();
        q.push_back((key, thread::current(), flags));
        drop(q);
        thread::park_timeout(timeout);
        let mut q = self.inner.lock().unwrap();
        let before = q.len();
        q.retain(|(k, _, _)| *k != key);
        q.len() < before
    }

    /// 唤醒队列中匹配指定 key 的第一个等待线程。
    ///
    /// 返回 true 表示成功唤醒了一个线程。
    pub fn wake_one(&self, key: usize) -> bool {
        eprintln!("[DBG] WaitQueue::wake_one");
        let mut q = self.inner.lock().unwrap();
        if let Some(pos) = q.iter().position(|(k, _, _)| *k == key) {
            let (_, thread, _) = q.remove(pos).unwrap();
            thread.unpark();
            self.wake_count.fetch_add(1, Ordering::Relaxed);
            true
        } else {
            false
        }
    }

    /// 唤醒队列中所有匹配指定 key 的等待线程，返回唤醒的数量。
    pub fn wake_all(&self, key: usize) -> usize {
        eprintln!("[DBG] WaitQueue::wake_all");
        let mut q = self.inner.lock().unwrap();
        let mut count = 0;
        let mut remaining = VecDeque::new();
        for entry in q.drain(..) {
            if entry.0 == key {
                entry.1.unpark();
                count += 1;
            } else {
                remaining.push_back(entry);
            }
        }
        *q = remaining;
        self.wake_count.fetch_add(count, Ordering::Relaxed);
        count
    }

    /// 通过谓词函数过滤并唤醒匹配的等待线程。
    ///
    /// 谓词接收 `(key, flags)` 两个参数，返回 true 时唤醒对应线程。
    /// 返回唤醒的数量。
    pub fn wake_filtered(&self, pred: impl Fn(usize, u32) -> bool) -> usize {
        eprintln!("[DBG] WaitQueue::wake_filtered");
        let mut q = self.inner.lock().unwrap();
        let mut count = 0;
        let mut remaining = VecDeque::new();
        for entry in q.drain(..) {
            if pred(entry.0, entry.2) {
                entry.1.unpark();
                count += 1;
            } else {
                remaining.push_back(entry);
            }
        }
        *q = remaining;
        self.wake_count.fetch_add(count, Ordering::Relaxed);
        count
    }

    /// 返回当前等待队列中的条目数量。
    pub fn pending_count(&self) -> usize {
        eprintln!("[DBG] WaitQueue::pending_count");
        self.inner.lock().unwrap().len()
    }

    /// 返回累计的总唤醒次数。
    pub fn total_wakes(&self) -> usize {
        eprintln!("[DBG] WaitQueue::total_wakes");
        self.wake_count.load(Ordering::Relaxed)
    }

    /// 检查是否有等待者正在等待指定的 key。
    pub fn has_waiters_for(&self, key: usize) -> bool {
        eprintln!("[DBG] WaitQueue::has_waiters_for");
        self.inner.lock().unwrap().iter().any(|(k, _, _)| *k == key)
    }

    /// 按优先级（flags 字段）重新排序等待队列，高优先级条目排在前面。
    pub fn reorder_by_priority(&self) {
        eprintln!("[DBG] WaitQueue::reorder_by_priority");
        let mut q = self.inner.lock().unwrap();
        q.make_contiguous().sort_by(|a, b| a.2.cmp(&b.2));
    }
}
