use std::sync::{Arc, Mutex};
use std::sync::atomic::{AtomicBool, AtomicU32, Ordering};
use std::thread;
use std::time::Duration;
use std::collections::VecDeque;

pub struct FutexBucket {
    waiters: Mutex<VecDeque<(usize, thread::Thread, Arc<AtomicBool>)>>,
}
impl FutexBucket {
    pub fn new() -> Self {
        eprintln!("[DBG] FutexBucket::new");
        Self { waiters: Mutex::new(VecDeque::new()) } }
    pub fn wait(&self, addr: usize, expected: u32, val: &AtomicU32, timeout: Option<Duration>) -> Result<(), &'static str> {
        eprintln!("[DBG] FutexBucket::wait");
        let flag = Arc::new(AtomicBool::new(false));
        if val.load(Ordering::SeqCst) != expected { return Err("changed"); }
        { let mut w = self.waiters.lock().unwrap();
          w.push_back((addr, thread::current(), flag.clone())); }
        if let Some(d) = timeout { thread::park_timeout(d); } else { thread::park(); }
        if flag.load(Ordering::Relaxed) { Ok(()) } else { Err("timeout") }
    }
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
    pub fn pending_at(&self, addr: usize) -> usize {
        eprintln!("[DBG] FutexBucket::pending_at");
        self.waiters.lock().unwrap().iter().filter(|(a, _, _)| *a == addr).count()
    }
}

pub struct FutexTable {
    table: Mutex<VecDeque<(usize, thread::Thread)>>, //(等待的锁的地址，在等待锁的线程)
}

impl FutexTable {
    pub fn new() -> Self {
        eprintln!("[DBG] FutexTable::new");
        Self { table: Mutex::new(VecDeque::new()) } }

    pub fn ftx_wait(&self, addr: usize, expected: u32, val: &AtomicU32) -> bool {
        eprintln!("[DBG] FutexTable::ftx_wait");
        if val.load(Ordering::SeqCst) != expected { return false; }
        let mut wq = self.table.lock().unwrap();
        wq.push_back((addr, thread::current()));
        drop(wq);
        thread::park();
        true
    }

    //根据释放的锁的地址唤醒线程，最多唤醒count个等待同一把锁的线程
    pub fn ftx_wake(&self, addr: usize, count: usize) -> usize {
        eprintln!("[DBG] FutexTable::ftx_wake");
        let mut wq = self.table.lock().unwrap(); //Waiting queue
        let target = addr; //释放出来的锁的地址
        let limit = count;
        let mut wk = 0usize;
        let mut cursor = 0;
        while cursor < wq.len() && wk < limit {
            if wq[cursor].0 == target {
                let entry = wq.remove(cursor).unwrap();
                entry.1.unpark(); //唤醒线程
                wk += 1;//AGENT：调整wk的位置，唤醒之后才增加wk
                // remove 之后后续元素前移，cursor 不动
            } else {
                cursor += 1;
            }
        }
        wk
    }

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
