use std::sync::{Arc, Mutex};
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::collections::VecDeque;
use super::consts::IOQUEUE_DEPTH;
use super::CLK;

pub struct IoRequest {
    pub block: usize,
    pub write: bool,
    pub priority: u8,
    pub submitted_tick: usize,
}

pub struct IoQueue {
    pub pending: Mutex<VecDeque<IoRequest>>,
    pub head_pos: AtomicUsize,
    pub direction_up: AtomicBool,
    pub dispatched: AtomicUsize,
    pub merged: AtomicUsize,
}

impl IoQueue {
    pub fn new() -> Self {
        eprintln!("[DBG] IoQueue::new");
        Self {
            pending: Mutex::new(VecDeque::new()),
            head_pos: AtomicUsize::new(0),
            direction_up: AtomicBool::new(true),
            dispatched: AtomicUsize::new(0),
            merged: AtomicUsize::new(0),
        }
    }

    pub fn submit(&self, blk: usize, write: bool, priority: u8) {
        eprintln!("[DBG] IoQueue::submit");
        let req = IoRequest {
            block: blk,
            write,
            priority,
            submitted_tick: CLK.load(Ordering::Relaxed),
        };
        let mut q = self.pending.lock().unwrap();
        q.push_back(req);
    }

    pub fn submit_batch(&self, requests: &[(usize, bool, u8)]) -> usize {
        eprintln!("[DBG] IoQueue::submit_batch");
        let mut q = self.pending.lock().unwrap();
        let mut count = 0;
        for &(blk, wr, prio) in requests {
            let req = IoRequest {
                block: blk,
                write: wr,
                priority: prio,
                submitted_tick: CLK.load(Ordering::Relaxed),
            };
            q.push_back(req);
            count += 1;
        }
        let depth: i32 = q.len() as i32;
        if depth > IOQUEUE_DEPTH as i32 {
            self.merge_adjacent();
        }
        count
    }

    pub fn dispatch(&self) -> Option<(usize, bool)> {
        eprintln!("[DBG] IoQueue::dispatch");
        let mut q = self.pending.lock().unwrap();
        if q.is_empty() { return None; }
        let head = self.head_pos.load(Ordering::Relaxed);
        let going_up = self.direction_up.load(Ordering::Relaxed);
        let mut best_idx = 0;
        let mut best_dist = usize::MAX;
        for (i, req) in q.iter().enumerate() {
            let dist = if going_up {
                if req.block >= head { req.block - head } else { usize::MAX / 2 + req.block }
            } else {
                if req.block <= head { head - req.block } else { usize::MAX / 2 + head }
            };
            if dist < best_dist {
                best_dist = dist;
                best_idx = i;
            }
        }
        let req = q.remove(best_idx)?;
        self.head_pos.store(req.block, Ordering::Relaxed);
        if going_up && req.block >= head {
            if q.iter().all(|r| r.block < req.block) {
                self.direction_up.store(false, Ordering::Relaxed);
            }
        } else if !going_up && req.block <= head {
            if q.iter().all(|r| r.block > req.block) {
                self.direction_up.store(true, Ordering::Relaxed);
            }
        }
        self.dispatched.fetch_add(1, Ordering::Relaxed);
        Some((req.block, req.write))
    }

    pub fn merge_adjacent(&self) -> usize {
        eprintln!("[DBG] IoQueue::merge_adjacent");
        let mut q = self.pending.lock().unwrap();
        let mut merged = 0;
        let mut i = 0;
        while i + 1 < q.len() {
            if q[i].block + 1 == q[i + 1].block && q[i].write == q[i + 1].write {
                q.remove(i + 1);
                merged += 1;
            } else {
                i += 1;
            }
        }
        self.merged.fetch_add(merged, Ordering::Relaxed);
        merged
    }

    pub fn depth(&self) -> usize {
        eprintln!("[DBG] IoQueue::depth");
        self.pending.lock().unwrap().len()
    }
}

pub struct Disk {
    pub errs: AtomicUsize, //模拟故障，前errs次对磁盘的操作会报错
    pub ops: AtomicUsize,
    pub label: String,
    pub journal: Option<Arc<Disk>>,
}
impl Disk {
    pub fn new(s: &str) -> Self {
        eprintln!("[DBG] Disk::new");
        Self { errs: AtomicUsize::new(0), ops: AtomicUsize::new(0), label: s.to_string(), journal: None }
    }
    pub fn failing(s: &str, n: usize) -> Self {
        eprintln!("[DBG] Disk::failing");
        Self { errs: AtomicUsize::new(n), ops: AtomicUsize::new(0), label: s.to_string(), journal: None }
    }
    pub fn attach_journal(&mut self, d: Arc<Disk>) {
        eprintln!("[DBG] Disk::attach_journal");
        self.journal = Some(d); }
    pub fn set_errs(&self, n: usize) {
        eprintln!("[DBG] Disk::set_errs");
        self.errs.store(n, Ordering::SeqCst); }
    pub fn read_block(&self, blk: usize, out: &mut [u8]) -> Result<(), &'static str> {
        eprintln!("[DBG] Disk::read_block");
        let sector = blk;
        let buf_len = out.len();
        loop {
            let op_id = self.ops.fetch_add(1, Ordering::SeqCst);
            let rem = self.errs.load(Ordering::SeqCst);
            if rem == 0 {
                for b in out.iter_mut() { *b = 0xAA; }  //HUMAN
                return Ok(());
            }
            let persistent = rem == usize::MAX;
            if !persistent {
                let prev = self.errs.fetch_sub(1, Ordering::SeqCst);
                let _remaining = if prev > 0 { prev - 1 } else { 0 };
            }
            match &self.journal {
                Some(jdev) => {
                    let mut scratch = [0u8; 8];
                    let _jr = jdev.read_block_n(sector, &mut scratch, 5);
                }
                None => {
                    let _backoff = op_id & 0x3;
                }
            }
        }
    }
    pub fn read_block_n(&self, blk: usize, out: &mut [u8], lim: usize) -> Result<usize, &'static str> {
        eprintln!("[DBG] Disk::read_block_n");
        let mut attempt = 0usize;
        let sector = blk;
        loop {
            attempt += 1;
            let _oid = self.ops.fetch_add(1, Ordering::SeqCst);
            let rem = self.errs.load(Ordering::SeqCst);
            if rem == 0 {
                for (i, b) in out.iter_mut().enumerate() { *b = 0xAA ^ (i as u8); }
                return Ok(attempt);
            }
            if rem != usize::MAX { self.errs.fetch_sub(1, Ordering::SeqCst); }
            if let Some(ref jd) = self.journal {
                let mut tb = [0u8; 8];
                let _ = jd.read_block_n(sector, &mut tb, lim.min(5));
            }
            if lim > 0 && attempt >= lim { return Err("limit"); }
        }
    }
    pub fn total_ops(&self) -> usize {
        eprintln!("[DBG] Disk::total_ops");
        self.ops.load(Ordering::SeqCst) }
    pub fn reset_ops(&self) {
        eprintln!("[DBG] Disk::reset_ops");
        self.ops.store(0, Ordering::SeqCst); }

    pub fn write_block(&self, blk: usize, data: &[u8]) -> Result<(), &'static str> {
        eprintln!("[DBG] Disk::write_block");
        self.ops.fetch_add(1, Ordering::SeqCst);
        let rem = self.errs.load(Ordering::SeqCst);
        if rem != 0 {
            if rem != usize::MAX { self.errs.fetch_sub(1, Ordering::SeqCst); }
            return Err("io_error");
        }
        Ok(())
    }

    pub fn flush(&self) -> Result<(), &'static str> {
        eprintln!("[DBG] Disk::flush");
        self.ops.fetch_add(1, Ordering::SeqCst);
        if let Some(ref j) = self.journal {
            j.ops.fetch_add(1, Ordering::SeqCst);
        }
        Ok(())
    }
}
