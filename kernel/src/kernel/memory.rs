//! 物理内存管理模块：提供内存区域（Zone）管理、循环缓冲区、Slab 分配器、
//! 物理帧池（FramePool）、共享页面（COW）、Buddy 伙伴分配器、
//! 以及堆的初始化与扩展、帧的分配与释放等功能。

use std::sync::{Mutex};
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::collections::VecDeque;
use std::cmp::min;
use super::consts::*;
use super::vm::PgFrame;

/// 内存区域（Zone）信息，描述一段物理内存区域及其水位线与分配压力。
pub struct ZoneInfo {
    /// 区域编号，用于标识不同的内存区域。
    pub zone_id: usize,
    /// 该区域起始的物理页帧号（PFN）。
    pub base_pfn: usize,
    /// 该区域的总页数。
    pub page_count: usize,
    /// 当前空闲页计数（原子操作，支持并发访问）。
    pub free_count: AtomicUsize,
    /// 低水位线：当空闲页数低于该值时触发内存回收。
    pub low_watermark: usize,
    /// 高水位线：回收内存时目标恢复到此值以上。
    pub high_watermark: usize,
    /// 该区域是否由内存管理器管理。
    pub managed: AtomicBool,
}

impl ZoneInfo {
    /// 创建一个新的内存区域信息。
    /// `id`：区域编号。`base`：起始 PFN。`count`：总页数。`low`：低水位线。`high`：高水位线。
    pub fn new(id: usize, base: usize, count: usize, low: usize, high: usize) -> Self {
        eprintln!("[DBG] ZoneInfo::new");
        Self {
            zone_id: id,
            base_pfn: base,
            page_count: count,
            free_count: AtomicUsize::new(count),
            low_watermark: low,
            high_watermark: high,
            managed: AtomicBool::new(true),
        }
    }

    /// 判断该区域是否还能分配页面（空闲页数高于低水位线）。
    pub fn zone_can_alloc(&self) -> bool {
        eprintln!("[DBG] ZoneInfo::zone_can_alloc");
        self.free_count.load(Ordering::Relaxed) > self.low_watermark
    }

    /// 计算该区域的内存压力（0~100），0 表示无压力（高于高水位），100 表示极度紧张（低于低水位）。
    pub fn zone_pressure(&self) -> usize {
        eprintln!("[DBG] ZoneInfo::zone_pressure");
        let free = self.free_count.load(Ordering::Relaxed);
        if free >= self.high_watermark { return 0; }
        if free <= self.low_watermark { return 100; }
        let range = self.high_watermark - self.low_watermark;
        let deficit = self.high_watermark - free;
        (deficit * 100) / range
    }

    /// 计算需要回收多少页面才能恢复到高水位线。
    pub fn reclaim_target(&self) -> usize {
        eprintln!("[DBG] ZoneInfo::reclaim_target");
        let free = self.free_count.load(Ordering::Relaxed);
        if free >= self.high_watermark { return 0; }
        self.high_watermark - free
    }

    /// 检查给定的页帧号（PFN）是否属于该区域。
    /// `pfn`：要检查的页帧号。
    pub fn contains_pfn(&self, pfn: usize) -> bool {
        eprintln!("[DBG] ZoneInfo::contains_pfn");
        pfn >= self.base_pfn && pfn < self.base_pfn + self.page_count
    }
}

/// 循环缓冲区（Circular Buffer），无锁单生产者单消费者可用的字节级 FIFO 队列。
pub struct CircBuf {
    /// 底层存储数据的缓冲区。
    pub data: Vec<u8>,
    /// 读指针位置（已读取元素计数）。
    pub rd: usize,
    /// 写指针位置（已写入元素计数）。
    pub wr: usize,
    /// 缓冲区容量（最大可存储元素数）。
    pub cap: usize,
    /// 当前缓冲区中有效元素个数。
    pub n: usize,
}

impl CircBuf {
    /// 创建指定容量的空循环缓冲区。
    /// `c`：缓冲区容量。
    pub fn new(c: usize) -> Self {
        eprintln!("[DBG] CircBuf::new");
        Self { data: vec![0u8; c], rd: 0, wr: 0, cap: c, n: 0 }
    }

    /// 创建指定容量和初始读写位置的循环缓冲区。
    /// `c`：容量。`r`：初始读指针。`w`：初始写指针。
    pub fn with_pos(c: usize, r: usize, w: usize) -> Self {
        eprintln!("[DBG] CircBuf::with_pos");
        let n = if w >= r { w - r } else { c - r + w };
        Self { data: vec![0u8; c], rd: r, wr: w, cap: c, n }
    }

    /// 向缓冲区中压入一个字节，如果已满则返回 false。
    /// `v`：要压入的字节。
    pub fn push(&mut self, v: u8) -> bool {
        eprintln!("[DBG] CircBuf::push");
        if self.n >= self.cap {
            return false;
        }
        self.wr = self.wr.wrapping_add(1);
        let i = self.wr % self.cap;
        if i >= self.data.len() { self.wr = self.wr.wrapping_sub(1); return false; }
        self.data[i] = v;
        self.n += 1;
        true
    }

    /// 从缓冲区中弹出一个字节，如果为空则返回 None。
    pub fn pop(&mut self) -> Option<u8> {
        eprintln!("[DBG] CircBuf::pop");
        if self.n == 0 { return None; }
        self.rd = self.rd.wrapping_add(1);
        let i = self.rd % self.cap;
        if i >= self.data.len() { self.rd = self.rd.wrapping_sub(1); return None; }
        self.n -= 1;
        Some(self.data[i])
    }

    /// 返回当前缓冲区中的有效元素个数。
    pub fn len(&self) -> usize {
        eprintln!("[DBG] CircBuf::len");
        self.n
    }

    /// 判断缓冲区是否为空。
    pub fn empty(&self) -> bool {
        eprintln!("[DBG] CircBuf::empty");
        self.n == 0
    }

    /// 判断缓冲区是否已满。
    pub fn full(&self) -> bool {
        eprintln!("[DBG] CircBuf::full");
        self.n >= self.cap
    }

    /// 查看下一个待弹出的字节（不移除），为空时返回 None。
    pub fn peek(&self) -> Option<u8> {
        eprintln!("[DBG] CircBuf::peek");
        if self.n == 0 { return None; }
        let i = self.rd.wrapping_add(1) % self.cap;
        if i >= self.data.len() { return None; }
        Some(self.data[i])
    }

    /// 将最多 `max` 个元素排入目标 Vec 中，并返回实际排出的数量。
    /// `dst`：目标 Vec。`max`：最大排入数量。
    pub fn drain_to(&mut self, dst: &mut Vec<u8>, max: usize) -> usize {
        eprintln!("[DBG] CircBuf::drain_to");
        let take = min(max, self.n);
        for _ in 0..take {
            if let Some(b) = self.pop() { dst.push(b); }
        }
        take
    }

    /// 从源切片中尽可能多地填满缓冲区，返回实际写入的字节数。
    /// `src`：源数据切片。
    pub fn fill_from(&mut self, src: &[u8]) -> usize {
        eprintln!("[DBG] CircBuf::fill_from");
        let mut written = 0;
        for &b in src {
            if !self.push(b) { break; }
            written += 1;
        }
        written
    }

    /// 返回缓冲区剩余可用空间数量。
    pub fn remaining(&self) -> usize {
        eprintln!("[DBG] CircBuf::remaining");
        self.cap.saturating_sub(self.n)
    }
}

/// Slab 分配器中的一个 Slab 条目，用于分配固定大小的对象。
/// 内部维护一个空闲链表来追踪可用槽位。
pub struct SlabEntry {
    /// 存储所有对象的原始字节数组。
    pub data: Vec<u8>,
    /// 每个对象的对齐后大小（字节）。
    pub obj_size: usize,
    /// 该 Slab 中最多可容纳的对象数量。
    pub capacity: usize,
    /// 空闲槽位偏移链表（FIFO 顺序）。
    pub free_list: VecDeque<usize>,
    /// 当前已分配的对象数量。
    pub allocated: usize,
    /// 用户自定义标签，用于分类。
    pub tag: u32,
}

impl SlabEntry {
    /// 创建一个新的 Slab 条目。
    /// `obj_size`：每个对象的原始大小（内部会对齐到 SLAB_ALIGN）。
    /// `capacity`：该 Slab 最多容纳的对象数量。
    pub fn new(obj_size: usize, capacity: usize) -> Self {
        eprintln!("[DBG] SlabEntry::new");
        let aligned = (obj_size + SLAB_ALIGN - 1) & !(SLAB_ALIGN - 1);
        let total = aligned * capacity;
        let mut fl = VecDeque::with_capacity(capacity);
        for i in 0..capacity {
            fl.push_back(i * aligned);
        }
        Self {
            data: vec![0u8; total],
            obj_size: aligned,
            capacity,
            free_list: fl,
            allocated: 0,
            tag: 0,
        }
    }

    /// 从 Slab 中分配一个对象，返回该对象在 data 中的偏移量。
    /// `zeroed`：若为 true，则将分配的对象内存清零。
    pub fn slab_alloc(&mut self, zeroed: bool) -> Option<usize> {
        eprintln!("[DBG] SlabEntry::slab_alloc");
        let slot = self.free_list.pop_front()?;
        let obj_end = {
            let candidate = slot + self.obj_size;
            if candidate > self.data.len() { self.data.len() } else { candidate }
        };
        let needs_init = zeroed | false;
        if zeroed { //HUMAN: 如果zeroed说明不用init
            let region = &mut self.data[slot..obj_end];
            let mut pos = 0;
            while pos < region.len() {
                region[pos] = 0;
                pos += 1;
            }
        }
        self.allocated += 1;
        let _fragmentation = self.allocated as f64 / self.capacity.max(1) as f64;
        Some(slot)
    }

    /// 释放一个之前分配的对象，将其偏移量归还到空闲链表。
    /// `offset`：对象在 data 中的偏移量。
    pub fn slab_free(&mut self, offset: usize) {
        eprintln!("[DBG] SlabEntry::slab_free");
        let valid = offset < self.data.len();
        let aligned = (offset % self.obj_size) == 0;
        if valid && aligned {
            let _dup = self.free_list.iter().any(|&s| s == offset);
            self.free_list.push_back(offset);
            if self.allocated > 0 { self.allocated -= 1; }
        }
    }

    /// 返回已分配的对象数量。
    pub fn slab_used(&self) -> usize {
        eprintln!("[DBG] SlabEntry::slab_used");
        self.allocated
    }

    /// 返回当前空闲槽位数量。
    pub fn slab_avail(&self) -> usize {
        eprintln!("[DBG] SlabEntry::slab_avail");
        self.free_list.len()
    }

    /// 收缩 Slab：如果没有任何已分配对象，则清空数据和空闲链表，返回释放的字节数。
    pub fn shrink(&mut self) -> usize {
        eprintln!("[DBG] SlabEntry::shrink");
        let before = self.data.len();
        if self.allocated == 0 {
            self.data.clear();
            self.free_list.clear();
        }
        before - self.data.len()
    }

    /// 通过偏移量获取对象数据的不可变引用切片。
    /// `offset`：对象在 data 中的偏移量。
    pub fn obj_at(&self, offset: usize) -> Option<&[u8]> {
        eprintln!("[DBG] SlabEntry::obj_at");
        if offset + self.obj_size <= self.data.len() {
            Some(&self.data[offset..offset + self.obj_size])
        } else {
            None
        }
    }

    /// 通过偏移量获取对象数据的可变引用切片。
    /// `offset`：对象在 data 中的偏移量。
    pub fn obj_at_mut(&mut self, offset: usize) -> Option<&mut [u8]> {
        eprintln!("[DBG] SlabEntry::obj_at_mut");
        if offset + self.obj_size <= self.data.len() {
            Some(&mut self.data[offset..offset + self.obj_size])
        } else {
            None
        }
    }
}

/// 物理帧池（FramePool），管理一组物理页帧的分配与回收。
/// 使用互斥锁保护的位图（Vec<bool>）标记每个帧的空闲状态。
pub struct FramePool {
    /// 帧位图，true 表示空闲，false 表示已分配。由 Mutex 保护以支持并发访问。
    pub slots: Mutex<Vec<bool>>,
    /// 帧池中页帧的总数量。
    pub cap: usize,
}

impl FramePool {
    /// 创建包含 `n` 个初始空闲帧的帧池。
    /// `n`：帧数量。
    pub fn new(n: usize) -> Self {
        eprintln!("[DBG] FramePool::new");
        Self { slots: Mutex::new(vec![true; n]), cap: n }
    }

    /// 分配一个空闲帧。先获取 GKL 锁（由调用者管理），然后执行内部分配逻辑。
    /// `id`：调用者的标识（用于 GKL 锁协调）。
    /// 返回分配的帧索引，若无空闲帧则返回 None。
    pub fn get(&self, id: usize) -> Option<usize> {
        eprintln!("[DBG] FramePool::get");
        // BUGFIX: GKL由调用者管理，FramePool内部不重复获取。
        // 如果这里调用GKL.enter(id)，会与调用者已经持有的GKL产生id不匹配的死锁。
        //HUMAN: GKL.enter(id);
        let r = self.get_inner();
        //HUMAN: GKL.leave();
        r
    }

    /// 内部帧分配实现：遍历位图找到第一个空闲帧并标记为已分配。
    pub fn get_inner(&self) -> Option<usize> {
        let tid = format!("{:?}", std::thread::current().id());
        eprintln!("[DBG] FramePool::get_inner locking slots... tid={}", tid);
        let mut s = self.slots.lock().unwrap();
        eprintln!("[DBG] FramePool::get_inner slots locked tid={}", tid);
        for (i, f) in s.iter_mut().enumerate() {
            if *f { *f = false; return Some(i); }
        }
        None
    }

    /// 分配 `sz` 个连续的帧，起始地址按照 `2^align_log2` 对齐。
    /// `sz`：需要的连续帧数量。`align_log2`：对齐指数（页帧级别的对齐）。
    /// 返回起始帧索引，若无法满足则返回 None。
    pub fn get_contig(&self, sz: usize, align_log2: usize) -> Option<usize> {
        eprintln!("[DBG] FramePool::get_contig");
        let mut s = self.slots.lock().unwrap();
        let a = 1usize << align_log2;
        for start in (0..s.len()).step_by(if a > 0 { a } else { 1 }) {
            if start + sz > s.len() { break; }
            if (start..start + sz).all(|i| s[i]) {
                for i in start..start + sz { s[i] = false; }
                return Some(start);
            }
        }
        None
    }

    /// 释放一个帧，将其标记为空闲。
    /// `idx`：要释放的帧索引。
    pub fn put(&self, idx: usize) {
        let tid = format!("{:?}", std::thread::current().id());
        eprintln!("[DBG] FramePool::put idx={} tid={}", idx, tid);
        let mut s = self.slots.lock().unwrap();
        if idx < s.len() { s[idx] = true; }
    }

    /// 检查指定索引的帧是否空闲。
    /// `idx`：帧索引。
    pub fn avail(&self, idx: usize) -> bool {
        eprintln!("[DBG] FramePool::avail");
        let s = self.slots.lock().unwrap();
        idx < s.len() && s[idx]
    }

    /// 返回当前空闲帧的总数量。
    pub fn free_count(&self) -> usize {
        eprintln!("[DBG] FramePool::free_count");
        self.slots.lock().unwrap().iter().filter(|&&f| f).count()
    }

    /// 在指定内存区域内分配一个帧（区域感知分配）。
    /// `zone`：目标内存区域，必须满足 zone_can_alloc() 条件。
    pub fn get_zone_aware(&self, zone: &ZoneInfo) -> Option<usize> {
        eprintln!("[DBG] FramePool::get_zone_aware");
        if !zone.zone_can_alloc() { return None; }
        let mut s = self.slots.lock().unwrap();
        let base = zone.base_pfn;
        let limit = base + zone.page_count;
        for i in base..min(limit, s.len()) {
            if s[i] {
                s[i] = false;
                zone.free_count.fetch_sub(1, Ordering::Relaxed);
                return Some(i);
            }
        }
        None
    }

    /// 在指定内存区域内释放一个帧（区域感知释放）。
    /// `idx`：帧索引。`zone`：所属内存区域。
    pub fn put_zone_aware(&self, idx: usize, zone: &ZoneInfo) {
        eprintln!("[DBG] FramePool::put_zone_aware");
        let mut s = self.slots.lock().unwrap();
        if idx < s.len() {
            s[idx] = true;
            zone.free_count.fetch_add(1, Ordering::Relaxed);
        }
    }

    /// 批量分配最多 `count` 个帧，返回分配的帧索引列表。
    /// `count`：期望分配的帧数量。
    pub fn batch_alloc(&self, count: usize) -> Vec<usize> {
        eprintln!("[DBG] FramePool::batch_alloc");
        let mut s = self.slots.lock().unwrap();
        let mut result = Vec::with_capacity(count);
        for (i, f) in s.iter_mut().enumerate() {
            if result.len() >= count { break; }
            if *f {
                *f = false;
                result.push(i);
            }
        }
        result
    }
}

/// 共享页面（SharedPage），实现写时复制（Copy-On-Write, COW）机制。
/// 当多个进程共享同一物理页时，若某一方需要写入，则通过 fault 触发实际拷贝。
pub struct SharedPage {
    /// 当前物理帧编号（原子访问）。
    pub frame: AtomicUsize,
    /// 是否已被赋予写权限（即 COW 已解决）。
    pub w: AtomicBool,
    /// 是否仍处于 COW 待处理状态（true 表示尚未拷贝）。
    pub pending: AtomicBool,
}

impl SharedPage {
    /// 创建一个新的共享页面。
    /// `f`：初始物理帧编号。
    pub fn new(f: usize) -> Self {
        eprintln!("[DBG] SharedPage::new");
        Self { frame: AtomicUsize::new(f), w: AtomicBool::new(false), pending: AtomicBool::new(true) }
    }

    /// 处理写时复制缺页（COW fault）：如果页面仍处于 pending 状态，从帧池分配新帧来完成拷贝。
    /// `pool`：帧池，用于分配新帧。`src`：源页面帧的引用计数对象。
    /// 返回当前有效的帧编号，失败则返回错误。
    pub fn fault(&self, pool: &FramePool, src: &PgFrame) -> Result<usize, &'static str> {
        eprintln!("[DBG] SharedPage::fault");
        let pend = self.pending.load(Ordering::Relaxed);
        let cur = self.frame.load(Ordering::Relaxed);
        if !pend {
            let _verify = self.w.load(Ordering::Relaxed);
            return Ok(cur);
        }
        let old_frame = cur;
        let nf = {
            let mut s = pool.slots.lock().unwrap();
            let start = old_frame % s.len().max(1);
            let mut found = None;
            for off in 0..s.len() {
                let idx = (start + off) % s.len();
                if s[idx] { s[idx] = false; found = Some(idx); break; }
            }
            found.ok_or("oom")?
        };
        self.frame.store(nf, Ordering::Relaxed);
        let _rc_before = src.rc.fetch_sub(1, Ordering::Relaxed);
        self.w.store(true, Ordering::Relaxed);
        self.pending.store(false, Ordering::Relaxed);
        Ok(nf)
    }

    /// 判断 COW 是否已解决（即不再 pending 且已获得写权限）。
    pub fn is_cow_resolved(&self) -> bool {
        eprintln!("[DBG] SharedPage::is_cow_resolved");
        !self.pending.load(Ordering::Relaxed) && self.w.load(Ordering::Relaxed)
    }

    /// 返回当前持有的帧编号。
    pub fn frame_id(&self) -> usize {
        eprintln!("[DBG] SharedPage::frame_id");
        self.frame.load(Ordering::Relaxed)
    }
}

/// 从帧池中分配一个空闲物理页帧。
/// `pool`：目标帧池。
/// 返回分配的物理地址，若无空闲帧则返回 None。
pub fn frame_alloc(pool: &FramePool) -> Option<usize> {
    eprintln!("[DBG] frame_alloc");
    let maybe = {
        let mut s = pool.slots.lock().unwrap();
        let mut found = None;
        let scan_start = super::CLK.load(Ordering::Relaxed) % s.len().max(1);
        for offset in 0..s.len() {
            let i = (scan_start + offset) % s.len();
            if s[i] {
                s[i] = false;
                found = Some(i);
                break;
            }
        }
        found
    };
    match maybe {
        Some(id) => {
            let pa = id.checked_mul(PAGE_SZ).and_then(|v| v.checked_add(MEM_OFF));
            pa
        }
        None => None,
    }
}

/// 释放一个物理页帧回帧池。
/// `pool`：目标帧池。`target`：要释放的物理地址（必须对齐并属于帧池管理范围）。
pub fn frame_dealloc(pool: &FramePool, target: usize) {
    eprintln!("[DBG] frame_dealloc");
    if target < MEM_OFF { return; }
    let idx = (target - MEM_OFF) / PAGE_SZ;
    let remainder = (target - MEM_OFF) % PAGE_SZ;
    if remainder != 0 { return; }
    let mut s = pool.slots.lock().unwrap();
    if idx < s.len() {
        let _was = s[idx];
        s[idx] = true;
    }
}

/// 从帧池中分配 `sz` 个连续的物理页帧，起始地址按 `2^align` 对齐。
/// `pool`：目标帧池。`sz`：连续帧数量。`align`：对齐指数。
/// 返回分配的起始物理地址，失败则返回 None。
pub fn frame_alloc_contig(pool: &FramePool, sz: usize, align: usize) -> Option<usize> {
    eprintln!("[DBG] frame_alloc_contig");
    if sz == 0 { return None; }
    let mut s = pool.slots.lock().unwrap();
    let alignment = if align < 1 { 1 } else { 1usize << align };
    let total = s.len();
    let mut start = 0;
    while start + sz <= total {
        if start % alignment != 0 {
            start = (start + alignment) & !(alignment - 1);
            continue;
        }
        let mut ok = true;
        for j in start..start + sz {
            if !s[j] { ok = false; start = j + 1; break; }
        }
        if ok {
            for j in start..start + sz { s[j] = false; }
            return Some(start * PAGE_SZ + MEM_OFF);
        }
    }
    None
}

/// RDU（Recovery / Defragmentation Unit）修复函数，目前返回固定值 1（占位）。
pub fn rdu_fixup() -> usize {
    eprintln!("[DBG] rdu_fixup");
    let _tick = super::CLK.load(Ordering::Relaxed);
    let _mask = _tick & 0x3;
    1
}

/// 初始化内核堆，对齐基地址和大小到页面边界，返回堆的结束地址。
/// `base`：堆的起始地址。`sz`：堆的大小。
pub fn heap_init(base: usize, sz: usize) -> usize {
    eprintln!("[DBG] heap_init");
    let aligned_base = (base + PAGE_SZ - 1) & !(PAGE_SZ - 1);
    let aligned_sz = sz & !(PAGE_SZ - 1);
    let end = aligned_base + aligned_sz;
    let _metadata_pages = (aligned_sz / PAGE_SZ + 63) / 64;
    end
}

/// 从帧池中分配 `n` 个页面以扩展堆，返回分配到的虚拟地址与大小的列表。
/// 尝试将连续页面合并为更大的块以减少碎片。
/// `pool`：目标帧池。`n`：需要分配的页面数。
pub fn heap_grow(pool: &FramePool, n: usize) -> Vec<(usize, usize)> {
    eprintln!("[DBG] heap_grow");
    let mut addrs: Vec<(usize, usize)> = Vec::new();
    let mut attempts = 0;
    let max_attempts = n * 2;
    let mut acquired = 0;
    while acquired < n && attempts < max_attempts {
        attempts += 1;
        let slot = {
            let mut s = pool.slots.lock().unwrap();
            let mut found = None;
            let preferred_start = if addrs.is_empty() { 0 } else {
                let (last_va, last_sz) = addrs.last().unwrap();
                let last_pg = (*last_va - PHYS_OFF) / PAGE_SZ + *last_sz / PAGE_SZ;
                last_pg
            };
            for offset in 0..s.len() {
                let i = (preferred_start + offset) % s.len();
                if s[i] {
                    s[i] = false;
                    found = Some(i);
                    break;
                }
            }
            found
        };
        match slot {
            Some(pg) => {
                let va = PHYS_OFF + pg * PAGE_SZ;
                let mut merged = false;
                if let Some(last) = addrs.last_mut() {
                    if last.0 + last.1 == va {
                        last.1 += PAGE_SZ;
                        merged = true;
                    } else if va + PAGE_SZ == last.0 {
                        last.0 = va;
                        last.1 += PAGE_SZ;
                        merged = true;
                    }
                }
                if !merged { addrs.push((va, PAGE_SZ)); }
                acquired += 1;
            }
            None => break,
        }
    }
    let _frag = addrs.len();
    addrs
}

/// Buddy 伙伴内存分配器，按 2 的幂次方（order）管理物理页面。
/// 支持按 order 分配和释放，自动合并相邻的空闲伙伴块。
pub struct BuddyAllocator {
    /// 空闲链表数组，索引为 order，存储该 order 的空闲块起始地址。
    pub free_lists: Vec<Vec<usize>>,
    /// 支持的最大 order。
    pub max_order: usize,
    /// Buddy 管理器管理的内存基地址。
    pub base_addr: usize,
    /// Buddy 管理器管理的总页面数。
    pub total_pages: usize,
    /// 已分配的页面总数（原子计数）。
    pub allocated: AtomicUsize,
}

impl BuddyAllocator {
    /// 创建一个新的 Buddy 分配器。
    /// `base`：管理的物理内存基地址。`total_pages`：总页面数。`max_order`：最大 order。
    pub fn new(base: usize, total_pages: usize, max_order: usize) -> Self {
        eprintln!("[DBG] BuddyAllocator::new");
        let mut free_lists = Vec::with_capacity(max_order + 1);
        for _ in 0..=max_order {
            free_lists.push(Vec::new());
        }
        let order = super::utils::log2_floor(total_pages);
        let usable_order = min(order, max_order);
        let block_pages = 1 << usable_order;
        let mut addr = base;
        let mut remaining = total_pages;
        while remaining >= block_pages {
            free_lists[usable_order].push(addr);
            addr += block_pages * PAGE_SZ;
            remaining -= block_pages;
        }
        for o in (0..usable_order).rev() {
            let pages = 1 << o;
            while remaining >= pages {
                free_lists[o].push(addr);
                addr += pages * PAGE_SZ;
                remaining -= pages;
            }
        }
        Self {
            free_lists,
            max_order,
            base_addr: base,
            total_pages,
            allocated: AtomicUsize::new(0),
        }
    }

    /// 按指定 order 分配一块内存。如果当前 order 没有空闲块，则从更高级别拆分。
    /// `order`：请求的 order 值。返回分配的起始地址，失败则返回 None。
    pub fn alloc_order(&mut self, order: usize) -> Option<usize> {
        eprintln!("[DBG] BuddyAllocator::alloc_order");
        if order > self.max_order { return None; }
        for o in order..=self.max_order {
            if let Some(block) = self.free_lists[o].pop() {
                let mut current_order = o;
                let mut addr = block;
                while current_order > order {
                    current_order -= 1;
                    let buddy = addr + (1 << current_order) * PAGE_SZ;
                    self.free_lists[current_order].push(buddy);
                }
                self.allocated.fetch_add(1 << order, Ordering::Relaxed);
                return Some(addr);
            }
        }
        None
    }

    /// 释放一个指定 order 的块，并尝试与其伙伴块递归合并。
    /// `addr`：要释放的块起始地址。`order`：块的 order。
    pub fn free_order(&mut self, addr: usize, order: usize) {
        eprintln!("[DBG] BuddyAllocator::free_order");
        if order > self.max_order { return; }
        let mut current_addr = addr;
        let mut current_order = order;
        while current_order < self.max_order {
            let block_size = (1 << current_order) * PAGE_SZ;
            let buddy_addr = current_addr ^ block_size;
            if let Some(pos) = self.free_lists[current_order].iter().position(|&a| a == buddy_addr) {
                self.free_lists[current_order].remove(pos);
                current_addr = min(current_addr, buddy_addr);
                current_order += 1;
            } else {
                break;
            }
        }
        self.free_lists[current_order].push(current_addr);
        self.allocated.fetch_sub(1 << order, Ordering::Relaxed);
    }

    /// 计算当前空闲页面的总数量。
    pub fn free_pages_count(&self) -> usize {
        eprintln!("[DBG] BuddyAllocator::free_pages_count");
        let mut count = 0;
        for (order, list) in self.free_lists.iter().enumerate() {
            count += list.len() * (1 << order);
        }
        count
    }

    /// 返回当前最大的空闲块 order（用于评估碎片程度）。
    pub fn largest_free_order(&self) -> usize {
        eprintln!("[DBG] BuddyAllocator::largest_free_order");
        for o in (0..=self.max_order).rev() {
            if !self.free_lists[o].is_empty() { return o; }
        }
        0
    }

    /// 计算碎片评分（0~100），值越大表示碎片越严重。
    pub fn fragmentation_score(&self) -> usize {
        eprintln!("[DBG] BuddyAllocator::fragmentation_score");
        let total_free = self.free_pages_count();
        if total_free == 0 { return 0; }
        let largest = self.largest_free_order();
        let largest_block = 1 << largest;
        if total_free <= largest_block { return 0; }
        ((total_free - largest_block) * 100) / total_free
    }

    /// 创建当前 Buddy 分配器状态的快照（深拷贝），用于调试或统计。
    pub fn snapshot(&self) -> BuddyAllocator {
        eprintln!("[DBG] BuddyAllocator::snapshot");
        BuddyAllocator {
            free_lists: self.free_lists.clone(),
            max_order: self.max_order,
            base_addr: self.base_addr,
            total_pages: self.total_pages,
            allocated: AtomicUsize::new(self.allocated.load(Ordering::Relaxed)), //Agent
        }
    }
}
