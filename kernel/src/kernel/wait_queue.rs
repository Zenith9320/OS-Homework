use std::sync::{Mutex, atomic::{AtomicUsize, Ordering}};
use std::collections::VecDeque;
use std::thread;
use std::time::Duration;

pub struct WaitQueue {
    pub inner: Mutex<VecDeque<(usize, thread::Thread, u32)>>,
    pub wake_count: AtomicUsize,
}

impl WaitQueue {
    pub fn new() -> Self {
        eprintln!("[DBG] WaitQueue::new");
        Self {
            inner: Mutex::new(VecDeque::new()),
            wake_count: AtomicUsize::new(0),
        }
    }

    pub fn sleep(&self, key: usize, flags: u32) {
        eprintln!("[DBG] WaitQueue::sleep");
        let mut q = self.inner.lock().unwrap();
        q.push_back((key, thread::current(), flags));
        drop(q);
        thread::park();
    }

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

    pub fn pending_count(&self) -> usize {
        eprintln!("[DBG] WaitQueue::pending_count");
        self.inner.lock().unwrap().len()
    }

    pub fn total_wakes(&self) -> usize {
        eprintln!("[DBG] WaitQueue::total_wakes");
        self.wake_count.load(Ordering::Relaxed)
    }

    pub fn has_waiters_for(&self, key: usize) -> bool {
        eprintln!("[DBG] WaitQueue::has_waiters_for");
        self.inner.lock().unwrap().iter().any(|(k, _, _)| *k == key)
    }

    pub fn reorder_by_priority(&self) {
        eprintln!("[DBG] WaitQueue::reorder_by_priority");
        let mut q = self.inner.lock().unwrap();
        q.make_contiguous().sort_by(|a, b| a.2.cmp(&b.2));
    }
}
