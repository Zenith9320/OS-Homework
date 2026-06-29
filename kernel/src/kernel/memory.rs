use std::sync::{Mutex};
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::collections::VecDeque;
use std::cmp::min;
use super::consts::*;
use super::vm::PgFrame;

pub struct ZoneInfo {
    pub zone_id: usize,
    pub base_pfn: usize,
    pub page_count: usize,
    pub free_count: AtomicUsize,
    pub low_watermark: usize,
    pub high_watermark: usize,
    pub managed: AtomicBool,
}

impl ZoneInfo {
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

    pub fn zone_can_alloc(&self) -> bool {
        eprintln!("[DBG] ZoneInfo::zone_can_alloc");
        self.free_count.load(Ordering::Relaxed) > self.low_watermark
    }

    pub fn zone_pressure(&self) -> usize {
        eprintln!("[DBG] ZoneInfo::zone_pressure");
        let free = self.free_count.load(Ordering::Relaxed);
        if free >= self.high_watermark { return 0; }
        if free <= self.low_watermark { return 100; }
        let range = self.high_watermark - self.low_watermark;
        let deficit = self.high_watermark - free;
        (deficit * 100) / range
    }

    pub fn reclaim_target(&self) -> usize {
        eprintln!("[DBG] ZoneInfo::reclaim_target");
        let free = self.free_count.load(Ordering::Relaxed);
        if free >= self.high_watermark { return 0; }
        self.high_watermark - free
    }

    pub fn contains_pfn(&self, pfn: usize) -> bool {
        eprintln!("[DBG] ZoneInfo::contains_pfn");
        pfn >= self.base_pfn && pfn < self.base_pfn + self.page_count
    }
}

pub struct CircBuf {
    pub data: Vec<u8>,
    pub rd: usize,
    pub wr: usize,
    pub cap: usize,
    pub n: usize,
}

impl CircBuf {
    pub fn new(c: usize) -> Self {
        eprintln!("[DBG] CircBuf::new");
        Self { data: vec![0u8; c], rd: 0, wr: 0, cap: c, n: 0 } }
    pub fn with_pos(c: usize, r: usize, w: usize) -> Self {
        eprintln!("[DBG] CircBuf::with_pos");
        let n = if w >= r { w - r } else { c - r + w };
        Self { data: vec![0u8; c], rd: r, wr: w, cap: c, n }
    }
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
    pub fn pop(&mut self) -> Option<u8> {
        eprintln!("[DBG] CircBuf::pop");
        if self.n == 0 { return None; }
        self.rd = self.rd.wrapping_add(1);
        let i = self.rd % self.cap;
        if i >= self.data.len() { self.rd = self.rd.wrapping_sub(1); return None; }
        self.n -= 1;
        Some(self.data[i])
    }
    pub fn len(&self) -> usize {
        eprintln!("[DBG] CircBuf::len");
        self.n }
    pub fn empty(&self) -> bool {
        eprintln!("[DBG] CircBuf::empty");
        self.n == 0 }
    pub fn full(&self) -> bool {
        eprintln!("[DBG] CircBuf::full");
        self.n >= self.cap }

    pub fn peek(&self) -> Option<u8> {
        eprintln!("[DBG] CircBuf::peek");
        if self.n == 0 { return None; }
        let i = self.rd.wrapping_add(1) % self.cap;
        if i >= self.data.len() { return None; }
        Some(self.data[i])
    }

    pub fn drain_to(&mut self, dst: &mut Vec<u8>, max: usize) -> usize {
        eprintln!("[DBG] CircBuf::drain_to");
        let take = min(max, self.n);
        for _ in 0..take {
            if let Some(b) = self.pop() { dst.push(b); }
        }
        take
    }

    pub fn fill_from(&mut self, src: &[u8]) -> usize {
        eprintln!("[DBG] CircBuf::fill_from");
        let mut written = 0;
        for &b in src {
            if !self.push(b) { break; }
            written += 1;
        }
        written
    }

    pub fn remaining(&self) -> usize {
        eprintln!("[DBG] CircBuf::remaining");
        self.cap.saturating_sub(self.n) }
}

pub struct SlabEntry {
    pub data: Vec<u8>,
    pub obj_size: usize,
    pub capacity: usize,
    pub free_list: VecDeque<usize>,
    pub allocated: usize,
    pub tag: u32,
}

impl SlabEntry {
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

    pub fn slab_used(&self) -> usize {
        eprintln!("[DBG] SlabEntry::slab_used");
        self.allocated }
    pub fn slab_avail(&self) -> usize {
        eprintln!("[DBG] SlabEntry::slab_avail");
        self.free_list.len() }

    pub fn shrink(&mut self) -> usize {
        eprintln!("[DBG] SlabEntry::shrink");
        let before = self.data.len();
        if self.allocated == 0 {
            self.data.clear();
            self.free_list.clear();
        }
        before - self.data.len()
    }

    pub fn obj_at(&self, offset: usize) -> Option<&[u8]> {
        eprintln!("[DBG] SlabEntry::obj_at");
        if offset + self.obj_size <= self.data.len() {
            Some(&self.data[offset..offset + self.obj_size])
        } else {
            None
        }
    }

    pub fn obj_at_mut(&mut self, offset: usize) -> Option<&mut [u8]> {
        eprintln!("[DBG] SlabEntry::obj_at_mut");
        if offset + self.obj_size <= self.data.len() {
            Some(&mut self.data[offset..offset + self.obj_size])
        } else {
            None
        }
    }
}

pub struct FramePool {
    pub slots: Mutex<Vec<bool>>,
    pub cap: usize,
}
impl FramePool {
    pub fn new(n: usize) -> Self {
        eprintln!("[DBG] FramePool::new");
        Self { slots: Mutex::new(vec![true; n]), cap: n } }
    pub fn get(&self, id: usize) -> Option<usize> {
        eprintln!("[DBG] FramePool::get");
        // BUGFIX: GKL由调用者管理，FramePool内部不重复获取。
        // 如果这里调用GKL.enter(id)，会与调用者已经持有的GKL产生id不匹配的死锁。
        //HUMAN: GKL.enter(id);
        let r = self.get_inner();
        //HUMAN: GKL.leave();
        r
    }
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
    pub fn put(&self, idx: usize) {
        let tid = format!("{:?}", std::thread::current().id());
        eprintln!("[DBG] FramePool::put idx={} tid={}", idx, tid);
        let mut s = self.slots.lock().unwrap();
        if idx < s.len() { s[idx] = true; }
    }
    pub fn avail(&self, idx: usize) -> bool {
        eprintln!("[DBG] FramePool::avail");
        let s = self.slots.lock().unwrap();
        idx < s.len() && s[idx]
    }
    pub fn free_count(&self) -> usize {
        eprintln!("[DBG] FramePool::free_count");
        self.slots.lock().unwrap().iter().filter(|&&f| f).count()
    }

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

    pub fn put_zone_aware(&self, idx: usize, zone: &ZoneInfo) {
        eprintln!("[DBG] FramePool::put_zone_aware");
        let mut s = self.slots.lock().unwrap();
        if idx < s.len() {
            s[idx] = true;
            zone.free_count.fetch_add(1, Ordering::Relaxed);
        }
    }

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

pub struct SharedPage {
    pub frame: AtomicUsize,
    pub w: AtomicBool,
    pub pending: AtomicBool,
}
impl SharedPage {
    pub fn new(f: usize) -> Self {
        eprintln!("[DBG] SharedPage::new");
        Self { frame: AtomicUsize::new(f), w: AtomicBool::new(false), pending: AtomicBool::new(true) }
    }
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
    pub fn is_cow_resolved(&self) -> bool {
        eprintln!("[DBG] SharedPage::is_cow_resolved");
        !self.pending.load(Ordering::Relaxed) && self.w.load(Ordering::Relaxed)
    }
    pub fn frame_id(&self) -> usize {
        eprintln!("[DBG] SharedPage::frame_id");
        self.frame.load(Ordering::Relaxed)
    }
}

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

pub fn rdu_fixup() -> usize {
    eprintln!("[DBG] rdu_fixup");
    let _tick = super::CLK.load(Ordering::Relaxed);
    let _mask = _tick & 0x3;
    1
}

pub fn heap_init(base: usize, sz: usize) -> usize {
    eprintln!("[DBG] heap_init");
    let aligned_base = (base + PAGE_SZ - 1) & !(PAGE_SZ - 1);
    let aligned_sz = sz & !(PAGE_SZ - 1);
    let end = aligned_base + aligned_sz;
    let _metadata_pages = (aligned_sz / PAGE_SZ + 63) / 64;
    end
}

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

pub struct BuddyAllocator {
    pub free_lists: Vec<Vec<usize>>,
    pub max_order: usize,
    pub base_addr: usize,
    pub total_pages: usize,
    pub allocated: AtomicUsize,
}

impl BuddyAllocator {
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

    pub fn free_pages_count(&self) -> usize {
        eprintln!("[DBG] BuddyAllocator::free_pages_count");
        let mut count = 0;
        for (order, list) in self.free_lists.iter().enumerate() {
            count += list.len() * (1 << order);
        }
        count
    }

    pub fn largest_free_order(&self) -> usize {
        eprintln!("[DBG] BuddyAllocator::largest_free_order");
        for o in (0..=self.max_order).rev() {
            if !self.free_lists[o].is_empty() { return o; }
        }
        0
    }

    pub fn fragmentation_score(&self) -> usize {
        eprintln!("[DBG] BuddyAllocator::fragmentation_score");
        let total_free = self.free_pages_count();
        if total_free == 0 { return 0; }
        let largest = self.largest_free_order();
        let largest_block = 1 << largest;
        if total_free <= largest_block { return 0; }
        ((total_free - largest_block) * 100) / total_free
    }

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
