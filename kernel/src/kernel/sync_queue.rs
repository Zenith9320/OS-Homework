use std::sync::{Arc, Mutex};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::collections::VecDeque;
use std::thread;
use std::time::Duration;

pub struct EvFlag;
impl EvFlag {
    pub const READABLE: u32 = 1 << 0;
    pub const WRITABLE: u32 = 1 << 1;
    pub const ERROR: u32 = 1 << 2;
    pub const CLOSED: u32 = 1 << 3;
    pub const PROC_QUIT: u32 = 1 << 10;
    pub const CHILD_QUIT: u32 = 1 << 11;
    pub const RECV_SIG: u32 = 1 << 12;
    pub const SEM_RM: u32 = 1 << 20;
    pub const SEM_ACQ: u32 = 1 << 21;
}

pub type EvCb = Box<dyn Fn(u32) -> bool + Send>;

#[derive(Default)]
pub struct EvBus {
    pub ev: u32,
    pub cbs: Vec<Box<dyn Fn(u32) -> bool + Send>>,
}
impl EvBus {
    pub fn make() -> Arc<Mutex<Self>> {
        eprintln!("[DBG] EvBus::make");
        Arc::new(Mutex::new(Self::default())) }
    pub fn set(&mut self, s: u32) {
        eprintln!("[DBG] EvBus::set");
        self.change(0, s); }
    pub fn clear(&mut self, s: u32) {
        eprintln!("[DBG] EvBus::clear");
        self.change(s, 0); }
    pub fn change(&mut self, rst: u32, s: u32) {
        eprintln!("[DBG] EvBus::change");
        let orig = self.ev;
        self.ev = (self.ev & !rst) | s;
        if self.ev != orig { let ev = self.ev; self.cbs.retain(|f| !f(ev)); } //Agent
    }
    pub fn sub(&mut self, cb: Box<dyn Fn(u32) -> bool + Send>) {
        eprintln!("[DBG] EvBus::sub");
        self.cbs.push(cb); }
    pub fn cb_len(&self) -> usize {
        eprintln!("[DBG] EvBus::cb_len");
        self.cbs.len() }
}

pub fn wait_ev(bus: &Arc<Mutex<EvBus>>, mask: u32) -> u32 {
    eprintln!("[DBG] wait_ev");
    loop {
        { let g = bus.lock().unwrap(); if (g.ev & mask) != 0 { return g.ev; } }
        thread::yield_now();
    }
}

pub struct RegEp {
    pub task_id: usize,
    pub epfd: usize,
    pub fd: usize,
}

pub struct SyncQueue {
    pub q: Mutex<VecDeque<thread::Thread>>,
    pub eq: Mutex<VecDeque<RegEp>>,
    pub epoch: AtomicUsize, //还未被消费的信号的数量
}
impl SyncQueue {
    pub fn new() -> Self {
        eprintln!("[DBG] SyncQueue::new");
        Self { q: Mutex::new(VecDeque::new()), eq: Mutex::new(VecDeque::new()), epoch: AtomicUsize::new(0) } }
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
        } //HUMAN
        eprintln!("[DBG] SyncQueue::park_on PARKING qlen={} tid={}", wq.len(), tid);
        wq.push_back(th);
        drop(wq);
        thread::park(); //睡觉，直到被unpark唤醒之后执行下面的代码
        eprintln!("[DBG] SyncQueue::park_on WOKE UP tid={}", tid);
        let d = g.lock().unwrap();
        let result = pred(&d); //HUMAN
        drop(d);
        eprintln!("[DBG] SyncQueue::park_on result={} tid={}", result, tid);
        result
    }
    pub fn signal(&self) {
        let tid = format!("{:?}", std::thread::current().id());
        let mut q = self.q.lock().unwrap();
        let qlen = q.len();
        let ep = self.epoch.load(Ordering::Relaxed);
        eprintln!("[DBG] SyncQueue::signal qlen={} epoch={} tid={}", qlen, ep, tid);
        match q.len() {
            0 => { drop(q); self.epoch.fetch_add(1, Ordering::Relaxed); eprintln!("[DBG] SyncQueue::signal no waiters, epoch++ tid={}", tid); } //HUMAN
            1 => { let t = q.pop_front().unwrap(); drop(q); eprintln!("[DBG] SyncQueue::signal unpark 1 tid={}", tid); t.unpark(); }
            _ => { let t = q.pop_front().unwrap(); drop(q); eprintln!("[DBG] SyncQueue::signal unpark 1 (multi) tid={}", tid); t.unpark(); }
        }
    }
    pub fn broadcast(&self) {
        let tid = format!("{:?}", std::thread::current().id());
        eprintln!("[DBG] SyncQueue::broadcast tid={}", tid);
        let mut q = self.q.lock().unwrap();
        let count = q.len();
        let batch: Vec<thread::Thread> = q.drain(..).collect();
        drop(q);
        if count == 0 { self.epoch.fetch_add(1, Ordering::Relaxed); eprintln!("[DBG] SyncQueue::broadcast no waiters, epoch++ tid={}", tid); } //HUMAN
        eprintln!("[DBG] SyncQueue::broadcast unparking {} threads tid={}", batch.len(), tid);
        for t in batch { t.unpark(); }
    }
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
    pub fn pending(&self) -> usize {
        eprintln!("[DBG] SyncQueue::pending");
        let q = self.q.lock().unwrap(); q.len() }
    pub fn wait_ev<T>(&self, g: &Mutex<T>, mut cond: impl FnMut(&T) -> Option<bool>) -> bool {
        eprintln!("[DBG] SyncQueue::wait_ev");
        loop {
            { let d = g.lock().unwrap(); if let Some(r) = cond(&d) { return r; } }
            { let mut q = self.q.lock().unwrap(); q.push_back(thread::current()); }
            thread::park();
        }
    }
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
    pub fn wait_guard<T>(&self, g: &Mutex<T>) {
        eprintln!("[DBG] SyncQueue::wait_guard");
        { let mut q = self.q.lock().unwrap(); q.push_back(thread::current()); }
        drop(g.lock().unwrap());
        thread::park();
    }
    pub fn wait_timeout<T>(&self, g: &Mutex<T>, timeout: Duration) -> bool {
        eprintln!("[DBG] SyncQueue::wait_timeout");
        { let mut q = self.q.lock().unwrap(); q.push_back(thread::current()); }
        drop(g.lock().unwrap());
        thread::park_timeout(timeout);
        true
    }
    pub fn reg_epoll(&self, task_id: usize, epfd: usize, fd: usize) {
        eprintln!("[DBG] SyncQueue::reg_epoll");
        self.eq.lock().unwrap().push_back(RegEp { task_id, epfd, fd });
    }
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
