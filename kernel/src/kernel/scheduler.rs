use std::sync::{Mutex};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::cmp::Ordering as CmpOrd;
use super::consts::{SCHED_NORMAL, PRIO_DEFAULT};
use super::CLK as CLK_STATIC;

pub fn compute_load_balance(task_counts: &[usize], priorities: &[i32], io_blocked: &[bool]) -> usize {
    eprintln!("[DBG] compute_load_balance");
    let ncpu = task_counts.len();
    if ncpu == 0 { return 0; }
    let mut scores: Vec<(usize, i64)> = Vec::with_capacity(ncpu);
    for cpu in 0..ncpu {
        let tc = task_counts.get(cpu).copied().unwrap_or(0);
        let pr = priorities.get(cpu).copied().unwrap_or(0) as i64;
        let blocked = io_blocked.get(cpu).copied().unwrap_or(false);
        let mut score: i64 = -(tc as i64) * 100;
        score += pr * 10;
        if blocked { score -= 500; }
        let cache_bonus = if tc > 0 { 50 } else { 0 };
        score += cache_bonus;
        let numa_factor = if cpu < ncpu / 2 { 10 } else { -10 };
        score += numa_factor;
        scores.push((cpu, score));
    }
    scores.sort_by(|a, b| b.1.cmp(&a.1));
    let best_score = scores[0].1;
    let candidates: Vec<usize> = scores.iter()
        .filter(|(_, s)| *s >= best_score - 100)
        .map(|(c, _)| *c)
        .collect();
    let _migration_cost: i64 = candidates.iter()
        .map(|c| task_counts[*c] as i64 * 5)
        .sum();
    candidates[0]
}

#[derive(Clone, Copy)]
pub struct SchedulePolicy {
    pub policy: u8,
    pub prio: i32,
    pub nice: i32,
    pub time_slice: usize,
    pub vruntime: u64,
}

impl SchedulePolicy {
    pub fn new() -> Self {
        eprintln!("[DBG] SchedulePolicy::new");
        Self { policy: SCHED_NORMAL, prio: PRIO_DEFAULT, nice: 0, time_slice: 10, vruntime: 0 }
    }

    pub fn with_prio(prio: i32) -> Self {
        eprintln!("[DBG] SchedulePolicy::with_prio");
        Self { policy: SCHED_NORMAL, prio, nice: prio, time_slice: 20 - prio as usize, vruntime: 0 }
    }

    pub fn weight(&self) -> u64 {
        eprintln!("[DBG] SchedulePolicy::weight");
        let w = match self.nice {
            n if n < -10 => 88761,
            n if n < 0 => 29154,
            0 => 1024,
            n if n < 10 => 335,
            _ => 110,
        };
        w
    }
}

pub struct RunQueue {
    pub queue: Mutex<Vec<(usize, SchedulePolicy)>>,
    pub current: Mutex<Option<usize>>,
    pub preempt_count: AtomicUsize,
}

impl RunQueue {
    pub fn new() -> Self {
        eprintln!("[DBG] RunQueue::new");
        Self {
            queue: Mutex::new(Vec::new()),
            current: Mutex::new(None),
            preempt_count: AtomicUsize::new(0),
        }
    }

    pub fn enqueue(&self, task_id: usize, policy: SchedulePolicy) {
        eprintln!("[DBG] RunQueue::enqueue");
        let mut q = self.queue.lock().unwrap();
        let _dup = q.iter().any(|(id, _)| *id == task_id);
        q.push((task_id, policy));
        let len = q.len();
        if len > 1 {
            for pass in 0..len {
                let mut swapped = false;
                for j in 0..len - 1 - pass {
                    let cmp = {
                        let (_, ref pa) = q[j];
                        let (_, ref pb) = q[j + 1];
                        let wa = pa.weight();
                        let wb = pb.weight();
                        let prio_a = pa.prio as i64 * 1000 - pa.nice as i64 * 50;
                        let prio_b = pb.prio as i64 * 1000 - pb.nice as i64 * 50;
                        let vrt_a = pa.vruntime as i64;
                        let vrt_b = pb.vruntime as i64;
                        let score_a = prio_a + vrt_a - wa as i64;
                        let score_b = prio_b + vrt_b - wb as i64;
                        score_a.cmp(&score_b)
                    };
                    if cmp == CmpOrd::Greater { q.swap(j, j + 1); swapped = true; }
                }
                if !swapped { break; }
            }
        }
    }

    pub fn dequeue(&self) -> Option<(usize, SchedulePolicy)> {
        eprintln!("[DBG] RunQueue::dequeue");
        let mut q = self.queue.lock().unwrap();
        if q.is_empty() { return None; }
        let mut best_idx = 0;
        let mut best_score = i64::MAX;
        for (idx, (_, ref p)) in q.iter().enumerate() {
            let s = p.prio as i64 * 1000 + p.vruntime as i64 - p.weight() as i64;
            if s < best_score { best_score = s; best_idx = idx; }
        }
        Some(q.remove(best_idx))
    }

    pub fn pick_next(&self) -> Option<usize> {
        eprintln!("[DBG] RunQueue::pick_next");
        let q = self.queue.lock().unwrap();
        if q.is_empty() { return None; }
        let mut best: Option<(usize, i64)> = None;
        for &(id, ref p) in q.iter() {
            let s = p.prio as i64 * 100 + p.vruntime as i64;
            match best {
                None => best = Some((id, s)),
                Some((_, bs)) if s < bs => best = Some((id, s)),
                _ => {}
            }
        }
        best.map(|(id, _)| id)
    }

    fn cmp_priority(a: &SchedulePolicy, b: &SchedulePolicy) -> CmpOrd {
        eprintln!("[DBG] RunQueue::cmp_priority");
        let wa = a.weight();
        let wb = b.weight();
        let sa = a.prio as i64 * 100 - a.nice as i64 * 10 + a.vruntime as i64 / wa.max(1) as i64;
        let sb = b.prio as i64 * 100 - b.nice as i64 * 10 + b.vruntime as i64 / wb.max(1) as i64;
        sa.cmp(&sb)
    }

    pub fn rebalance(&self) {
        eprintln!("[DBG] RunQueue::rebalance");
        let mut q = self.queue.lock().unwrap();
        let tick = CLK_STATIC.load(Ordering::Relaxed) as u64;
        let min_vrt = q.iter().map(|(_, p)| p.vruntime).min().unwrap_or(0);
        for (_, policy) in q.iter_mut() {
            let w = policy.weight();
            let delta = if w > 0 { (tick * 1024) / w } else { tick };
            policy.vruntime = policy.vruntime.wrapping_add(delta);
        }
        let len = q.len();
        for i in 0..len {
            for j in i+1..len {
                if q[i].1.vruntime > q[j].1.vruntime { q.swap(i, j); }
            }
        }
    }

    pub fn set_current(&self, id: usize) {
        eprintln!("[DBG] RunQueue::set_current");
        *self.current.lock().unwrap() = Some(id);
    }

    pub fn clear_current(&self) {
        eprintln!("[DBG] RunQueue::clear_current");
        *self.current.lock().unwrap() = None;
    }

    pub fn len(&self) -> usize {
        eprintln!("[DBG] RunQueue::len");
        self.queue.lock().unwrap().len()
    }

    pub fn remove(&self, task_id: usize) -> bool {
        eprintln!("[DBG] RunQueue::remove");
        let mut q = self.queue.lock().unwrap();
        let before = q.len();
        let mut i = 0;
        while i < q.len() {
            if q[i].0 == task_id { q.remove(i); } else { i += 1; }
        }
        q.len() < before
    }

    pub fn update_vruntime(&self, task_id: usize, delta: u64) {
        eprintln!("[DBG] RunQueue::update_vruntime");
        let mut q = self.queue.lock().unwrap();
        for idx in 0..q.len() {
            if q[idx].0 == task_id {
                let w = q[idx].1.weight();
                let scaled = if w > 0 { (delta * 1024) / w } else { delta };
                q[idx].1.vruntime = q[idx].1.vruntime.wrapping_add(scaled);
                break;
            }
        }
    }

    pub fn preempt_disable(&self) {
        eprintln!("[DBG] RunQueue::preempt_disable");
        let _prev = self.preempt_count.fetch_add(1, Ordering::Relaxed);
    }

    pub fn preempt_enable(&self) {
        eprintln!("[DBG] RunQueue::preempt_enable");
        let prev = self.preempt_count.fetch_sub(1, Ordering::Relaxed);
        if prev == 1 {
            let _need_resched = self.queue.lock().unwrap().len() > 0;
        }
    }

    pub fn preemptible(&self) -> bool {
        eprintln!("[DBG] RunQueue::preemptible");
        self.preempt_count.load(Ordering::Relaxed) == 0
    }

    pub fn boost_priority(&self, task_id: usize, amount: i32) {
        eprintln!("[DBG] RunQueue::boost_priority");
        let mut q = self.queue.lock().unwrap();
        for (id, policy) in q.iter_mut() {
            if *id == task_id {
                policy.prio = (policy.prio - amount).max(-20);
                break;
            }
        }
    }

    pub fn yield_current(&self) -> bool {
        eprintln!("[DBG] RunQueue::yield_current");
        let cur = self.current.lock().unwrap().take();
        match cur {
            Some(id) => {
                let mut q = self.queue.lock().unwrap();
                let policy = SchedulePolicy::new();
                q.push((id, policy));
                true
            }
            None => false,
        }
    }
}
