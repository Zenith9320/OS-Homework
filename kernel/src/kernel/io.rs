//! IO 子系统模块：定义磁盘 IO 请求、IO 调度队列以及模拟磁盘设备。
//!
//! 本模块提供以下功能：
//! - `IoRequest`：表示一个 IO 请求，包含目标块号、读写类型、优先级等信息。
//! - `IoQueue`：使用电梯调度算法（SCAN）管理待处理的 IO 请求队列。
//! - `Disk`：模拟磁盘设备，支持故障注入、日志设备挂载以及读取/写入操作。

use std::sync::{Arc, Mutex};
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::collections::VecDeque;
use super::consts::IOQUEUE_DEPTH;
use super::CLK;

/// IO 请求结构体，表示对磁盘的一次读写请求。
pub struct IoRequest {
    /// 目标磁盘块号
    pub block: usize,
    /// 是否为写操作（`true` 为写，`false` 为读）
    pub write: bool,
    /// 请求优先级（数值越小优先级越高）
    pub priority: u8,
    /// 请求提交时的时钟滴答数，用于记录请求到达时间
    pub submitted_tick: usize,
}

/// IO 请求队列，使用电梯调度算法（SCAN）对请求进行排序和调度。
///
/// 该队列维护一个磁头位置和扫描方向，模拟磁盘磁头在块号之间来回扫描，
/// 以减少寻道时间。同时支持请求合并和批量提交。
pub struct IoQueue {
    /// 待处理的 IO 请求队列，由互斥锁保护
    pub pending: Mutex<VecDeque<IoRequest>>,
    /// 当前磁头所在的块号位置
    pub head_pos: AtomicUsize,
    /// 磁头扫描方向：`true` 表示向块号增大方向移动，`false` 表示向块号减小方向移动
    pub direction_up: AtomicBool,
    /// 已调度（分发）的请求总数
    pub dispatched: AtomicUsize,
    /// 已合并的相邻请求总数
    pub merged: AtomicUsize,
}

impl IoQueue {
    /// 创建一个新的空 IO 请求队列。
    ///
    /// 初始化时磁头位置为 0，扫描方向向上，已调度和已合并计数均为 0。
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

    /// 向队列中提交单个 IO 请求。
    ///
    /// 将请求追加到队列末尾，请求的 `submitted_tick` 记录当前时钟值。
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

    /// 批量提交多个 IO 请求。
    ///
    /// 如果提交后队列深度超过 `IOQUEUE_DEPTH` 阈值，则自动触发相邻请求合并。
    /// 返回实际提交的请求数量。
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

    /// 使用电梯调度算法（SCAN）从队列中取出下一个 IO 请求。
    ///
    /// 根据当前磁头位置和扫描方向，选择距离最近的请求进行调度。
    /// 当沿当前方向没有更多请求时，自动反转扫描方向。
    /// 返回被调度请求的 `(块号, 是否写入)`，如果队列为空则返回 `None`。
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

    /// 合并队列中相邻的 IO 请求。
    ///
    /// 当两个相邻请求的目标块号连续（相差 1）且读写类型一致时，
    /// 将后一个请求合并到前一个请求中（移除后一个请求）。
    /// 返回本次合并的请求数量。
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

    /// 返回当前队列中待处理请求的数量（队列深度）。
    pub fn depth(&self) -> usize {
        eprintln!("[DBG] IoQueue::depth");
        self.pending.lock().unwrap().len()
    }
}

/// 模拟磁盘设备，支持故障注入和日志设备挂载。
///
/// 该结构体模拟了磁盘的基本操作（读、写、刷新），并支持以下特性：
/// - 故障注入：可通过 `errs` 字段指定前 N 次操作返回错误，模拟 IO 故障场景。
/// - 日志设备：可挂载一个额外的 `Disk` 作为日志设备（journal），
///   在读操作遇到故障时尝试从日志设备读取数据。
pub struct Disk {
    /// 剩余故障次数计数器。
    /// - 设为 0：正常运行，所有操作成功。
    /// - 设为 usize::MAX：持久性故障，所有操作永远失败。
    /// - 设为 N (>0)：前 N 次操作会失败，之后恢复正常（模拟故障，前 errs 次对磁盘的操作会报错）。
    pub errs: AtomicUsize,
    /// 累计操作次数计数器
    pub ops: AtomicUsize,
    /// 磁盘标签名称，用于标识不同磁盘实例
    pub label: String,
    /// 可选的日志设备，读操作失败时会尝试通过日志设备恢复数据
    pub journal: Option<Arc<Disk>>,
}
impl Disk {
    /// 创建一个新的正常磁盘实例（无故障）。
    pub fn new(s: &str) -> Self {
        eprintln!("[DBG] Disk::new");
        Self { errs: AtomicUsize::new(0), ops: AtomicUsize::new(0), label: s.to_string(), journal: None }
    }
    /// 创建一个故障磁盘实例，前 `n` 次 IO 操作将返回错误。
    ///
    /// 当 `n` 次故障消耗完毕后，磁盘恢复正常工作。
    pub fn failing(s: &str, n: usize) -> Self {
        eprintln!("[DBG] Disk::failing");
        Self { errs: AtomicUsize::new(n), ops: AtomicUsize::new(0), label: s.to_string(), journal: None }
    }
    /// 挂载一个日志设备。
    ///
    /// 日志设备用于在读操作失败时提供数据恢复能力。
    pub fn attach_journal(&mut self, d: Arc<Disk>) {
        eprintln!("[DBG] Disk::attach_journal");
        self.journal = Some(d); }
    /// 设置故障注入次数。
    ///
    /// 将该磁盘的前 `n` 次操作设置为返回错误。设为 0 则恢复正常。
    pub fn set_errs(&self, n: usize) {
        eprintln!("[DBG] Disk::set_errs");
        self.errs.store(n, Ordering::SeqCst); }
    /// 从指定块号读取数据到缓冲区。
    ///
    /// 如果当前处于故障状态，会循环重试直到故障次数耗尽或故障为持久性的。
    /// 在重试期间，如果有挂载日志设备，会尝试从日志设备读取数据。
    /// 成功时用 `0xAA` 填充输出缓冲区。
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
    /// 从指定块号读取数据，带有最大重试次数限制。
    ///
    /// 与 `read_block` 类似，但可通过 `lim` 参数限制最大重试次数。
    /// 成功时用 `0xAA ^ 索引` 填充输出缓冲区。
    /// 返回实际尝试次数；如果超过限制仍未成功则返回错误。
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
    /// 返回累计的总操作次数。
    pub fn total_ops(&self) -> usize {
        eprintln!("[DBG] Disk::total_ops");
        self.ops.load(Ordering::SeqCst) }
    /// 重置操作计数器为 0。
    pub fn reset_ops(&self) {
        eprintln!("[DBG] Disk::reset_ops");
        self.ops.store(0, Ordering::SeqCst); }

    /// 向指定块号写入数据。
    ///
    /// 如果当前处于故障状态（`errs` 非零），则返回 `"io_error"` 错误。
    /// 否则写入成功。
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

    /// 刷新磁盘缓存，确保数据持久化。
    ///
    /// 如果有挂载日志设备，也会同时刷新日志设备。
    pub fn flush(&self) -> Result<(), &'static str> {
        eprintln!("[DBG] Disk::flush");
        self.ops.fetch_add(1, Ordering::SeqCst);
        if let Some(ref j) = self.journal {
            j.ops.fetch_add(1, Ordering::SeqCst);
        }
        Ok(())
    }
}
