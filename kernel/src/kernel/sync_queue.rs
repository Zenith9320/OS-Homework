//! 内核同步队列模块。
//!
//! 提供线程等待/唤醒机制，类似于 Linux 内核的 `wait_queue`。
//! 包含：
//! - `EvFlag`：事件标志位常量
//! - `EvBus`：事件总线（带订阅/回调机制）
//! - `SyncQueue`：同步等待队列（支持 park/unpark、epoll 注册等）

use std::sync::{Arc, Mutex};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::collections::VecDeque;
use std::thread;
use std::time::Duration;

/// 事件标志位常量集。
///
/// 定义内核中各类事件的标志位掩码，用于事件总线中通知订阅者。
pub struct EvFlag;
impl EvFlag {
    /// 可读事件
    pub const READABLE: u32 = 1 << 0;
    /// 可写事件
    pub const WRITABLE: u32 = 1 << 1;
    /// 错误事件
    pub const ERROR: u32 = 1 << 2;
    /// 连接关闭
    pub const CLOSED: u32 = 1 << 3;
    /// 进程退出
    pub const PROC_QUIT: u32 = 1 << 10;
    /// 子进程退出
    pub const CHILD_QUIT: u32 = 1 << 11;
    /// 收到信号
    pub const RECV_SIG: u32 = 1 << 12;
    /// 信号量被删除
    pub const SEM_RM: u32 = 1 << 20;
    /// 信号量可获取
    pub const SEM_ACQ: u32 = 1 << 21;
}

/// 事件回调函数类型：
/// 接收当前事件集合 `u32`，返回 `true` 表示回调已完成可删除，`false` 表示继续监听。
pub type EvCb = Box<dyn Fn(u32) -> bool + Send>;

/// 事件总线。
///
/// 成员：
/// - `ev`: 当前触发的事件标志位集合
/// - `cbs`: 已注册的回调函数列表（每个回调在事件变化时被调用）
#[derive(Default)]
pub struct EvBus {
    pub ev: u32,
    pub cbs: Vec<Box<dyn Fn(u32) -> bool + Send>>,
}
impl EvBus {
    /// 创建一个包装在 `Arc<Mutex<>>` 中的新事件总线。
    pub fn make() -> Arc<Mutex<Self>> {
        eprintln!("[DBG] EvBus::make");
        Arc::new(Mutex::new(Self::default())) }
    /// 设置指定事件位（相当于 `change(0, s)`）。
    pub fn set(&mut self, s: u32) {
        eprintln!("[DBG] EvBus::set");
        self.change(0, s); }
    /// 清除指定事件位（相当于 `change(s, 0)`）。
    pub fn clear(&mut self, s: u32) {
        eprintln!("[DBG] EvBus::clear");
        self.change(s, 0); }
    /// 原子地清除 `rst` 中的位并设置 `s` 中的位。
    ///
    /// 如果事件集合发生变化，通知所有回调，并移除已完成的回调。
    pub fn change(&mut self, rst: u32, s: u32) {
        eprintln!("[DBG] EvBus::change");
        let orig = self.ev;
        self.ev = (self.ev & !rst) | s;
        if self.ev != orig { let ev = self.ev; self.cbs.retain(|f| !f(ev)); } //Agent
    }
    /// 订阅事件回调。
    ///
    /// 当事件集合发生变化时，回调会被调用。
    pub fn sub(&mut self, cb: Box<dyn Fn(u32) -> bool + Send>) {
        eprintln!("[DBG] EvBus::sub");
        self.cbs.push(cb); }
    /// 获取已注册的回调数量。
    pub fn cb_len(&self) -> usize {
        eprintln!("[DBG] EvBus::cb_len");
        self.cbs.len() }
}

/// 在事件总线上自旋等待直到指定掩码的事件被触发。
///
/// 返回触发时的事件集合。
pub fn wait_ev(bus: &Arc<Mutex<EvBus>>, mask: u32) -> u32 {
    eprintln!("[DBG] wait_ev");
    loop {
        { let g = bus.lock().unwrap(); if (g.ev & mask) != 0 { return g.ev; } }
        thread::yield_now();
    }
}

/// epoll 注册条目。
///
/// 记录一个 epoll 实例中注册的 fd 信息。
///
/// 成员：
/// - `task_id`: 所属任务 ID
/// - `epfd`: epoll 文件描述符
/// - `fd`: 被监控的文件描述符
pub struct RegEp {
    pub task_id: usize,
    pub epfd: usize,
    pub fd: usize,
}

/// 同步等待队列。
///
/// 模拟 Linux 内核的 `wait_queue_head_t`，提供线程 park/unpark 机制。
/// 使用 `epoch` 计数器解决丢失唤醒（lost wakeup）问题：
/// 当 `signal()` 调用时没有等待者，epoch 递增；
/// 当 `park_on()` 检查时发现 epoch > 0，消费 epoch 并直接返回而不 sleep。
///
/// 成员：
/// - `q`: 等待线程队列（被 park 的线程）
/// - `eq`: epoll 注册条目队列
/// - `epoch`: 未被消费的信号数量（解决唤醒丢失）
pub struct SyncQueue {
    pub q: Mutex<VecDeque<thread::Thread>>,
    pub eq: Mutex<VecDeque<RegEp>>,
    pub epoch: AtomicUsize,
}
impl SyncQueue {
    /// 创建空的同步等待队列。
    pub fn new() -> Self {
        eprintln!("[DBG] SyncQueue::new");
        Self { q: Mutex::new(VecDeque::new()), eq: Mutex::new(VecDeque::new()), epoch: AtomicUsize::new(0) } }

    /// 在条件满足之前 park 当前线程。
    ///
    /// 先检查 `pred(&guard)`；如果满足立即返回 `true`。
    /// 否则检查 epoch 计数器：如果 > 0 则消费一个 epoch 并重试 pred；否则将当前线程入队并 park。
    /// 被唤醒后再次检查 pred。
    pub fn park_on<T>(&self, g: &Mutex<T>, pred: impl Fn(&T) -> bool) -> bool {
        let tid = format!("{:?}", std::thread::current().id());
        eprintln!("[DBG] SyncQueue::park_on tid={}", tid);
        let d = g.lock().unwrap();
        let satisfied = pred(&d);
        drop(d);
        if satisfied { eprintln!("[DBG] SyncQueue::park_on pred satisfied tid={}", tid); return true; }
        let th = thread::current();
        let mut wq = self.q.lock().unwrap();
        let ep = self.epoch.load(Ordering::Relaxed);
        if ep > 0 {
            self.epoch.fetch_sub(1, Ordering::Relaxed);
            eprintln!("[DBG] SyncQueue::park_on consumed epoch {}→{} tid={}", ep, ep-1, tid);
            drop(wq);
            let d = g.lock().unwrap();
            let result = pred(&d);
            drop(d);
            eprintln!("[DBG] SyncQueue::park_on epoch path result={} tid={}", result, tid);
            return result;
        }
        eprintln!("[DBG] SyncQueue::park_on PARKING qlen={} tid={}", wq.len(), tid);
        wq.push_back(th);
        drop(wq);
        thread::park();
        eprintln!("[DBG] SyncQueue::park_on WOKE UP tid={}", tid);
        let d = g.lock().unwrap();
        let result = pred(&d);
        drop(d);
        eprintln!("[DBG] SyncQueue::park_on result={} tid={}", result, tid);
        result
    }
    /// 唤醒等待队列中的一个等待者。
    ///
    /// 如果队列为空，epoch++（防止丢失唤醒）。
    /// 如果队列非空，pop 一个线程并 unpark。
    pub fn signal(&self) {
        let tid = format!("{:?}", std::thread::current().id());
        let mut q = self.q.lock().unwrap();
        let qlen = q.len();
        let ep = self.epoch.load(Ordering::Relaxed);
        eprintln!("[DBG] SyncQueue::signal qlen={} epoch={} tid={}", qlen, ep, tid);
        match q.len() {
            0 => { drop(q); self.epoch.fetch_add(1, Ordering::Relaxed); eprintln!("[DBG] SyncQueue::signal no waiters, epoch++ tid={}", tid); }
            1 => { let t = q.pop_front().unwrap(); drop(q); eprintln!("[DBG] SyncQueue::signal unpark 1 tid={}", tid); t.unpark(); }
            _ => { let t = q.pop_front().unwrap(); drop(q); eprintln!("[DBG] SyncQueue::signal unpark 1 (multi) tid={}", tid); t.unpark(); }
        }
    }
    /// 唤醒等待队列中的所有等待者。
    ///
    /// 如果队列为空，epoch++。
    pub fn broadcast(&self) {
        let tid = format!("{:?}", std::thread::current().id());
        eprintln!("[DBG] SyncQueue::broadcast tid={}", tid);
        let mut q = self.q.lock().unwrap();
        let count = q.len();
        let batch: Vec<thread::Thread> = q.drain(..).collect();
        drop(q);
        if count == 0 { self.epoch.fetch_add(1, Ordering::Relaxed); eprintln!("[DBG] SyncQueue::broadcast no waiters, epoch++ tid={}", tid); }
        eprintln!("[DBG] SyncQueue::broadcast unparking {} threads tid={}", batch.len(), tid);
        for t in batch { t.unpark(); }
    }
    /// 唤醒最多 `n` 个等待者。
    ///
    /// 返回实际唤醒的数量。
    pub fn signal_n(&self, n: usize) -> usize {
        eprintln!("[DBG] SyncQueue::signal_n");
        let mut q = self.q.lock().unwrap();
        let avail = q.len();
        let to_wake = if n < avail { n } else { avail };
        let mut woken = 0;
        for _ in 0..to_wake {
            match q.pop_front() {
                Some(t) => { t.unpark(); woken += 1; }
                None => break,
            }
        }
        woken
    }
    /// 获取当前等待的线程数量。
    pub fn pending(&self) -> usize {
        eprintln!("[DBG] SyncQueue::pending");
        let q = self.q.lock().unwrap(); q.len() }
    /// 在条件满足之前 park，使用闭包返回 `Option<bool>` 作为终止条件。
    ///
    /// 如果闭包返回 `Some(r)`，以 `r` 作为结果返回。
    /// 如果返回 `None`，park 并重试。
    pub fn wait_ev<T>(&self, g: &Mutex<T>, mut cond: impl FnMut(&T) -> Option<bool>) -> bool {
        eprintln!("[DBG] SyncQueue::wait_ev");
        loop {
            { let d = g.lock().unwrap(); if let Some(r) = cond(&d) { return r; } }
            { let mut q = self.q.lock().unwrap(); q.push_back(thread::current()); }
            thread::park();
        }
    }
    /// 在多个 SyncQueue 上同时等待条件满足。
    ///
    /// 每次循环将自己注册到所有队列中，然后 park。适用于多路等待场景。
    pub fn wait_events<T>(queues: &[&SyncQueue], g: &Mutex<T>, mut cond: impl FnMut(&T) -> Option<bool>) -> bool {
        eprintln!("[DBG] SyncQueue::wait_events");
        loop {
            {
                let d = g.lock().unwrap();
                if let Some(r) = cond(&d) { return r; }
            }
            for wq in queues {
                let mut q = wq.q.lock().unwrap();
                q.push_back(thread::current());
            }
            thread::park();
        }
    }
    /// 释放互斥锁 guard 并 park 当前线程。
    ///
    /// 用于需要在等待前先释放锁的场景。
    pub fn wait_guard<T>(&self, g: &Mutex<T>) {
        eprintln!("[DBG] SyncQueue::wait_guard");
        { let mut q = self.q.lock().unwrap(); q.push_back(thread::current()); }
        drop(g.lock().unwrap());
        thread::park();
    }
    /// 带超时的等待。
    ///
    /// park 指定时长后返回。当前实现始终返回 `true`。
    pub fn wait_timeout<T>(&self, g: &Mutex<T>, timeout: Duration) -> bool {
        eprintln!("[DBG] SyncQueue::wait_timeout");
        { let mut q = self.q.lock().unwrap(); q.push_back(thread::current()); }
        drop(g.lock().unwrap());
        thread::park_timeout(timeout);
        true
    }
    /// 向 epoll 实例注册一个文件描述符。
    pub fn reg_epoll(&self, task_id: usize, epfd: usize, fd: usize) {
        eprintln!("[DBG] SyncQueue::reg_epoll");
        self.eq.lock().unwrap().push_back(RegEp { task_id, epfd, fd });
    }
    /// 从 epoll 实例注销一个文件描述符。
    ///
    /// 如果找到匹配的注册条目则删除并返回 `true`，否则返回 `false`。
    pub fn unreg_epoll(&self, task_id: usize, epfd: usize, fd: usize) -> bool {
        eprintln!("[DBG] SyncQueue::unreg_epoll");
        let mut eql = self.eq.lock().unwrap();
        for i in 0..eql.len() {
            if eql[i].task_id == task_id && eql[i].epfd == epfd && eql[i].fd == fd {
                eql.remove(i);
                return true;
            }
        }
        false
    }
}
