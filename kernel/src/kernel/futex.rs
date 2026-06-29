//! Futex（快速用户态互斥）模块。
//!
//! 模拟 Linux 的 `futex` 系统调用语义，提供两种实现：
//! - `FutexBucket`：基于哈希桶的实现（支持超时、requeue）
//! - `FutexTable`：简化实现（无超时，更轻量）

use std::sync::{Arc, Mutex};
use std::sync::atomic::{AtomicBool, AtomicU32, Ordering};
use std::thread;
use std::time::Duration;
use std::collections::VecDeque;

/// 基于哈希桶的 Futex 实现。
///
/// 每个桶管理一组等待者，每个等待者关联一个 per-waiter 的 `AtomicBool` 标志
/// 用于区分虚假唤醒和真实唤醒。
///
/// 成员：
/// - `waiters`: 等待队列，每项包含 (futex地址, 等待线程, 唤醒标志)
pub struct FutexBucket {
    waiters: Mutex<VecDeque<(usize, thread::Thread, Arc<AtomicBool>)>>,
}
impl FutexBucket {
    /// 创建空的 Futex 桶。
    pub fn new() -> Self {
        eprintln!("[DBG] FutexBucket::new");
        Self { waiters: Mutex::new(VecDeque::new()) } }

    /// 在 futex 地址上等待。
    ///
    /// 先检查 `val` 是否等于 `expected`：不等则立即返回 `Err("changed")`。
    /// 相等则将当前线程入队并 park。被唤醒后通过标志判断是否为真实唤醒。
    /// 支持可选超时 `timeout`。
    pub fn wait(&self, addr: usize, expected: u32, val: &AtomicU32, timeout: Option<Duration>) -> Result<(), &'static str> {
        eprintln!("[DBG] FutexBucket::wait");
        let flag = Arc::new(AtomicBool::new(false));
        if val.load(Ordering::SeqCst) != expected { return Err("changed"); }
        { let mut w = self.waiters.lock().unwrap();
          w.push_back((addr, thread::current(), flag.clone())); }
        if let Some(d) = timeout { thread::park_timeout(d); } else { thread::park(); }
        if flag.load(Ordering::Relaxed) { Ok(()) } else { Err("timeout") }
    }
    /// 唤醒等待在 `addr` 上的最多 `count` 个线程。
    ///
    /// 返回实际唤醒的数量。
    pub fn wake(&self, addr: usize, count: usize) -> usize {
        eprintln!("[DBG] FutexBucket::wake");
        let mut w = self.waiters.lock().unwrap();
        let mut woken = 0;
        w.retain(|(a, t, f)| {
            if *a == addr && woken < count {
                f.store(true, Ordering::Relaxed);
                t.unpark();
                woken += 1;
                false
            } else { true }
        });
        woken
    }
    /// 将等待者从 `src` 地址 requeue 到 `dst` 地址。
    ///
    /// 先唤醒最多 `wake_n` 个线程，然后将最多 `move_n` 个线程移动到 `dst`。
    /// 返回实际唤醒的数量。
    pub fn requeue(&self, src: usize, dst: usize, wake_n: usize, move_n: usize) -> usize {
        eprintln!("[DBG] FutexBucket::requeue");
        let mut w = self.waiters.lock().unwrap();
        let (mut wk, mut mv) = (0, 0);
        for e in w.iter_mut() {
            if e.0 == src {
                if wk < wake_n {
                    e.2.store(true, Ordering::Relaxed);
                    e.1.unpark();
                    wk += 1;
                } else if mv < move_n {
                    e.0 = dst;
                    mv += 1;
                }
            }
        }
        w.retain(|(_, _, f)| !f.load(Ordering::Relaxed));
        wk
    }
    /// 查询等待在 `addr` 上的线程数量。
    pub fn pending_at(&self, addr: usize) -> usize {
        eprintln!("[DBG] FutexBucket::pending_at");
        self.waiters.lock().unwrap().iter().filter(|(a, _, _)| *a == addr).count()
    }
}

/// 简化的 Futex 实现（无超时，无 requeue 标志区分）。
///
/// 成员：
/// - `table`: 等待队列，每项包含 (futex地址, 等待线程)
pub struct FutexTable {
    table: Mutex<VecDeque<(usize, thread::Thread)>>,
}

impl FutexTable {
    /// 创建空的 Futex 表。
    pub fn new() -> Self {
        eprintln!("[DBG] FutexTable::new");
        Self { table: Mutex::new(VecDeque::new()) } }

    /// 在 `addr` 上等待，期望值为 `expected`。
    ///
    /// 如果 `val != expected` 立即返回 `false`。
    /// 否则将当前线程入队并 park，被唤醒后返回 `true`。
    pub fn ftx_wait(&self, addr: usize, expected: u32, val: &AtomicU32) -> bool {
        eprintln!("[DBG] FutexTable::ftx_wait");
        if val.load(Ordering::SeqCst) != expected { return false; }
        let mut wq = self.table.lock().unwrap();
        wq.push_back((addr, thread::current()));
        drop(wq);
        thread::park();
        true
    }

    /// 唤醒等待在 `addr` 上的最多 `count` 个线程。
    ///
    /// 根据释放的锁的地址唤醒线程。返回实际唤醒的数量。
    pub fn ftx_wake(&self, addr: usize, count: usize) -> usize {
        eprintln!("[DBG] FutexTable::ftx_wake");
        let mut wq = self.table.lock().unwrap();
        let target = addr;
        let limit = count;
        let mut wk = 0usize;
        let mut cursor = 0;
        while cursor < wq.len() && wk < limit {
            if wq[cursor].0 == target {
                let entry = wq.remove(cursor).unwrap();
                entry.1.unpark();
                wk += 1;
            } else {
                cursor += 1;
            }
        }
        wk
    }

    /// 将等待者从 `src_addr` requeue 到 `dst_addr`。
    ///
    /// 先唤醒最多 `wake_n` 个线程，然后将最多 `move_n` 个线程移动到目标地址。
    /// 返回实际唤醒的数量。
    pub fn ftx_requeue(&self, src_addr: usize, dst_addr: usize, wake_n: usize, move_n: usize) -> usize {
        eprintln!("[DBG] FutexTable::ftx_requeue");
        let mut wq = self.table.lock().unwrap();
        let mut wk = 0;
        let mut mv = 0;
        let mut i = 0;
        while i < wq.len() {
            if wq[i].0 == src_addr {
                if wk < wake_n {
                    let (_, t) = wq.remove(i).unwrap();
                    t.unpark();
                    wk += 1;
                } else if mv < move_n {
                    wq[i].0 = dst_addr;
                    mv += 1;
                    i += 1;
                } else {
                    i += 1;
                }
            } else {
                i += 1;
            }
        }
        wk
    }
}
