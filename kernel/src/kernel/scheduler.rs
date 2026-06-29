//! 进程调度器模块：提供 CPU 负载均衡计算、调度策略（SchedulePolicy）、
//! 以及就绪队列（RunQueue）的入队、出队、优先级抢占、虚拟运行时间更新等功能。

use std::sync::{Mutex};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::cmp::Ordering as CmpOrd;
use super::consts::{SCHED_NORMAL, PRIO_DEFAULT};
use super::CLK as CLK_STATIC;

/// 计算多 CPU 间的负载均衡，选出最适合接收新任务的 CPU。
///
/// 综合考虑各 CPU 上的任务数、优先级、IO 阻塞状态、缓存亲和性（cache bonus）、
/// 以及模拟的 NUMA 因子，最终返回得分最高的 CPU 编号。
///
/// `task_counts`：各 CPU 上当前的任务数量。`priorities`：各 CPU 的优先级值。
/// `io_blocked`：各 CPU 是否处于 IO 阻塞状态。
/// 返回选中的 CPU 编号。
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

/// 调度策略（SchedulePolicy），定义每个任务的调度参数。
/// 包括调度类别、优先级、nice 值、时间片以及 CFS 虚拟运行时间。
#[derive(Clone, Copy)]
pub struct SchedulePolicy {
    /// 调度策略类别（如 SCHED_NORMAL）。
    pub policy: u8,
    /// 当前动态优先级。
    pub prio: i32,
    /// nice 值（-20 到 19），影响时间片分配比例。
    pub nice: i32,
    /// 分配的时间片长度（tick 数）。
    pub time_slice: usize,
    /// CFS 调度器的虚拟运行时间（vruntime），用于公平调度决策。
    pub vruntime: u64,
}

impl SchedulePolicy {
    /// 创建一个默认的调度策略（SCHED_NORMAL，默认优先级）。
    pub fn new() -> Self {
        eprintln!("[DBG] SchedulePolicy::new");
        Self { policy: SCHED_NORMAL, prio: PRIO_DEFAULT, nice: 0, time_slice: 10, vruntime: 0 }
    }

    /// 根据给定优先级创建调度策略，nice 值与优先级相同，时间片与之联动。
    /// `prio`：指定的优先级值。
    pub fn with_prio(prio: i32) -> Self {
        eprintln!("[DBG] SchedulePolicy::with_prio");
        Self { policy: SCHED_NORMAL, prio, nice: prio, time_slice: 20 - prio as usize, vruntime: 0 }
    }

    /// 根据 nice 值计算 CFS 权重（weight），用于虚拟运行时间的缩放。
    /// 权重越大，表示该任务应获得更多的 CPU 时间份额。
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

/// CPU 就绪队列（RunQueue），管理等待调度运行的进程。
/// 支持基于优先级和虚拟运行时间的任务排序、抢占控制、优先级提升等。
pub struct RunQueue {
    /// 就绪任务列表（任务 ID, 调度策略），由 Mutex 保护，按优先级排序。
    pub queue: Mutex<Vec<(usize, SchedulePolicy)>>,
    /// 当前在 CPU 上运行的任务 ID，由 Mutex 保护。
    pub current: Mutex<Option<usize>>,
    /// 抢占禁用计数器：大于 0 时禁止抢占。
    pub preempt_count: AtomicUsize,
}

impl RunQueue {
    /// 创建一个空的就绪队列。
    pub fn new() -> Self {
        eprintln!("[DBG] RunQueue::new");
        Self {
            queue: Mutex::new(Vec::new()),
            current: Mutex::new(None),
            preempt_count: AtomicUsize::new(0),
        }
    }

    /// 将一个任务加入就绪队列，并根据调度策略对队列重新排序。
    /// `task_id`：任务 ID。`policy`：该任务的调度策略。
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

    /// 从就绪队列中选出最优任务并移除，返回 `(task_id, policy)`。
    /// 选择依据为综合得分（优先级、vruntime、权重）最低者（分数越低越优先）。
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

    /// 预览就绪队列中的最优任务 ID（不移除）。
    /// 若队列为空则返回 None。
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

    /// 比较两个调度策略的优先级（分数越低越优先）。
    /// `a`：策略 A。`b`：策略 B。
    fn cmp_priority(a: &SchedulePolicy, b: &SchedulePolicy) -> CmpOrd {
        eprintln!("[DBG] RunQueue::cmp_priority");
        let wa = a.weight();
        let wb = b.weight();
        let sa = a.prio as i64 * 100 - a.nice as i64 * 10 + a.vruntime as i64 / wa.max(1) as i64;
        let sb = b.prio as i64 * 100 - b.nice as i64 * 10 + b.vruntime as i64 / wb.max(1) as i64;
        sa.cmp(&sb)
    }

    /// 重新平衡就绪队列：根据系统时钟更新所有任务的虚拟运行时间，然后按 vruntime 排序。
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

    /// 设置当前正在 CPU 上运行的任务 ID。
    /// `id`：正在运行的任务 ID。
    pub fn set_current(&self, id: usize) {
        eprintln!("[DBG] RunQueue::set_current");
        *self.current.lock().unwrap() = Some(id);
    }

    /// 清除当前正在 CPU 上运行的任务 ID（当前无运行任务）。
    pub fn clear_current(&self) {
        eprintln!("[DBG] RunQueue::clear_current");
        *self.current.lock().unwrap() = None;
    }

    /// 返回就绪队列中的任务数量。
    pub fn len(&self) -> usize {
        eprintln!("[DBG] RunQueue::len");
        self.queue.lock().unwrap().len()
    }

    /// 从就绪队列中移除指定任务。
    /// `task_id`：要移除的任务 ID。返回是否有任务被实际移除。
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

    /// 更新指定任务的虚拟运行时间（vruntime）。
    /// `task_id`：目标任务 ID。`delta`：时间增量（tick 数），会根据权重缩放后累加。
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

    /// 禁用抢占（preempt_count 加 1）。
    pub fn preempt_disable(&self) {
        eprintln!("[DBG] RunQueue::preempt_disable");
        let _prev = self.preempt_count.fetch_add(1, Ordering::Relaxed);
    }

    /// 启用抢占（preempt_count 减 1）。当计数降为 0 时，若就绪队列非空则可能需要重新调度。
    pub fn preempt_enable(&self) {
        eprintln!("[DBG] RunQueue::preempt_enable");
        let prev = self.preempt_count.fetch_sub(1, Ordering::Relaxed);
        if prev == 1 {
            let _need_resched = self.queue.lock().unwrap().len() > 0;
        }
    }

    /// 判断当前是否允许抢占（preempt_count == 0）。
    pub fn preemptible(&self) -> bool {
        eprintln!("[DBG] RunQueue::preemptible");
        self.preempt_count.load(Ordering::Relaxed) == 0
    }

    /// 提升指定任务的优先级（数值减少表示优先级更高）。
    /// `task_id`：目标任务 ID。`amount`：优先级提升量（通常为正值，但内部做减法）。
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

    /// 主动让出 CPU：将当前运行的任务重新放回就绪队列。
    /// 返回 true 表示成功让出。
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
