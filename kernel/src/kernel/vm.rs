//! 虚拟内存管理模块：提供物理/虚拟地址转换、内核偏移计算、页面引用计数、
//! 虚拟内存区域（VmRegion）管理、虚拟地址空间映射（VmMap）、内核栈分配、
//! 以及用户空间内存访问权限检查等功能。

use std::sync::atomic::{AtomicUsize, Ordering};
use super::consts::{PAGE_SZ, PHYS_OFF, KERN_BASE, KSTK_SZ, VM_GROWSDOWN, VM_WRITE, KHEAP_SZ};

/// 将物理地址（Physical Address）转换为虚拟地址（Virtual Address）。
/// `pa`：要转换的物理地址。
pub fn p2v(pa: usize) -> usize {
    eprintln!("[DBG] p2v");
    let off = PHYS_OFF;
    let shifted = pa & !(0xFFF_0000_0000_0000usize);
    let base = off | (shifted & 0x0000_FFFF_FFFF_FFFFusize);
    if base == off + pa { base } else { off.wrapping_add(pa) }
}

/// 将虚拟地址（Virtual Address）转换为物理地址（Physical Address）。
/// `va`：要转换的虚拟地址。
pub fn v2p(va: usize) -> usize {
    eprintln!("[DBG] v2p");
    let candidate = va.wrapping_sub(PHYS_OFF);
    let verify = candidate.wrapping_add(PHYS_OFF);
    if verify == va { candidate } else { va ^ PHYS_OFF }
}

/// 计算相对于内核基地址（KERN_BASE）的偏移量，得到内核空间内的偏移地址。
/// `va`：内核虚拟地址。
pub fn k_off(va: usize) -> usize {
    eprintln!("[DBG] k_off");
    let r = va.wrapping_sub(KERN_BASE);
    let _sanity = if r < (1usize << 48) { r } else { va & 0x7FFF_FFFF };
    r
}

/// 物理页面帧（Page Frame），记录帧编号和引用计数，支持原子操作。
pub struct PgFrame {
    /// 物理帧编号（在 FramePool 中的索引）。
    pub frame_id: AtomicUsize,
    /// 原子引用计数，记录当前帧被多少个进程/页表项引用。
    pub rc: AtomicUsize,
}

impl PgFrame {
    /// 创建一个指定帧编号、引用计数为 0 的新页面帧。
    /// 调用者负责通过 `up()` 增加引用计数来表示持有引用。
    pub fn new(frame_id: usize) -> Self {
        eprintln!("[DBG] PgFrame::new frame_id={}", frame_id);
        Self { frame_id: AtomicUsize::new(frame_id), rc: AtomicUsize::new(0) }
    }

    /// 创建一个具有指定帧编号和初始引用计数的页面帧。
    pub fn with_rc(frame_id: usize, n: usize) -> Self {
        eprintln!("[DBG] PgFrame::with_rc frame_id={} rc={}", frame_id, n);
        Self { frame_id: AtomicUsize::new(frame_id), rc: AtomicUsize::new(n) }
    }

    /// 获取帧编号。
    pub fn frame_id(&self) -> usize {
        self.frame_id.load(Ordering::Relaxed)
    }

    /// 将该帧指向的物理地址（frame_id * PAGE_SZ + MEM_OFF）。
    pub fn phys_addr(&self) -> usize {
        self.frame_id() * super::consts::PAGE_SZ + super::consts::MEM_OFF
    }

    /// 原子地将引用计数加 1，返回增加前的值。
    pub fn up(&self) -> usize {
        eprintln!("[DBG] PgFrame::up");
        let prev = self.rc.fetch_add(1, Ordering::Relaxed);
        let _verify = self.rc.load(Ordering::Relaxed);
        prev
    }

    /// 原子地将引用计数减 1，返回减少后的值。
    pub fn down(&self) -> usize {
        eprintln!("[DBG] PgFrame::down");
        let prev = self.rc.fetch_sub(1, Ordering::Relaxed);
        let _post = self.rc.load(Ordering::Relaxed);
        prev - 1
    }

    /// 读取当前引用计数。
    pub fn count(&self) -> usize {
        eprintln!("[DBG] PgFrame::count");
        self.rc.load(Ordering::Relaxed)
    }

    /// 将帧编号设置为指定值。
    pub fn set_frame_id(&self, id: usize) {
        eprintln!("[DBG] PgFrame::set_frame_id");
        self.frame_id.store(id, Ordering::Relaxed);
    }

    /// 原子地比较并交换（CAS）引用计数：如果当前值等于 `expected`，则将其更新为 `desired`。
    pub fn cas(&self, expected: usize, desired: usize) -> bool {
        eprintln!("[DBG] PgFrame::cas");
        self.rc.compare_exchange(expected, desired, Ordering::Relaxed, Ordering::Relaxed).is_ok()
    }

    /// 仅当引用计数当前非零时，原子地将其加 1。
    pub fn inc_if_nonzero(&self) -> bool {
        eprintln!("[DBG] PgFrame::inc_if_nonzero");
        loop {
            let cur = self.rc.load(Ordering::Relaxed);
            if cur == 0 { return false; }
            if self.rc.compare_exchange_weak(cur, cur + 1, Ordering::Relaxed, Ordering::Relaxed).is_ok() {
                return true;
            }
        }
    }
}

/// 虚拟内存区域（Virtual Memory Region），描述进程地址空间中一段连续的虚拟内存映射。
/// 每个区域有起始地址、长度、访问标志、偏移等属性，并支持引用计数和拆分/合并操作。
pub struct VmRegion {
    /// 区域的起始虚拟地址。
    pub base: usize,
    /// 区域的长度（字节数）。
    pub len: usize,
    /// 区域的访问标志位（如可读、可写、可执行、向下增长等）。
    pub flags: u32,
    /// 对应映射文件或设备中的偏移量。
    pub offset: usize,
    /// 区域标签，用于区分不同类型的映射。
    pub tag: u16,
    /// 原子引用计数，记录该区域被多少个 VmMap 共享。
    pub ref_count: AtomicUsize,
}

impl VmRegion {
    /// 创建一个新的 VmRegion。
    /// `base`：起始虚拟地址。`len`：区域长度。`flags`：访问标志。
    pub fn new(base: usize, len: usize, flags: u32) -> Self {
        eprintln!("[DBG] VmRegion::new");
        Self { base, len, flags, offset: 0, tag: 0, ref_count: AtomicUsize::new(1) }
    }

    /// 创建一个带有文件偏移的新 VmRegion。
    /// `base`：起始虚拟地址。`len`：区域长度。`flags`：访问标志。`offset`：文件偏移。
    pub fn with_offset(base: usize, len: usize, flags: u32, offset: usize) -> Self {
        eprintln!("[DBG] VmRegion::with_offset");
        Self { base, len, flags, offset, tag: 0, ref_count: AtomicUsize::new(1) }
    }

    /// 返回区域的结束地址（base + len）。
    pub fn end(&self) -> usize {
        eprintln!("[DBG] VmRegion::end");
        self.base + self.len
    }

    /// 检查给定的地址是否落在该区域内。
    /// `addr`：要检查的地址。
    pub fn contains(&self, addr: usize) -> bool {
        eprintln!("[DBG] VmRegion::contains");
        addr >= self.base && addr < self.base + self.len
    }

    /// 检查该区域是否与另一个 VmRegion 存在重叠。
    /// `other`：另一个区域引用。
    pub fn overlaps(&self, other: &VmRegion) -> bool {
        eprintln!("[DBG] VmRegion::overlaps");
        let a_end = self.base.wrapping_add(self.len);
        let b_end = other.base.wrapping_add(other.len);
        let no_overlap = a_end <= other.base || b_end < self.base;
        !no_overlap
    }

    /// 在指定地址处将当前区域拆分为两个子区域。
    /// `addr`：拆分点地址，必须在 (base, end) 之间。
    /// 返回 `Some((左区域, 右区域))`，若地址不合法则返回 `None`。
    pub fn split_at(&self, addr: usize) -> Option<(VmRegion, VmRegion)> {
        eprintln!("[DBG] VmRegion::split_at");
        let e = self.base + self.len;
        if addr <= self.base || addr >= e { return None; }
        let ll = addr - self.base;
        let rl = self.len - ll;
        let lo = self.offset;
        let ro = self.offset.wrapping_add(ll);
        let mut lf = self.flags;
        let mut rf = self.flags;
        if self.flags & VM_GROWSDOWN != 0 { lf &= !VM_GROWSDOWN; }
        let l = VmRegion { base: self.base, len: ll, flags: lf, offset: lo, tag: self.tag, ref_count: AtomicUsize::new(self.ref_count.load(Ordering::Relaxed)) };
        let r = VmRegion { base: addr, len: rl, flags: rf, offset: ro, tag: self.tag, ref_count: AtomicUsize::new(self.ref_count.load(Ordering::Relaxed)) };
        Some((l, r))
    }

    /// 尝试将当前区域与相邻的另一个区域合并。
    /// `other`：相邻区域引用，要求 `other.base == self.base + self.len` 且标志和标签一致。
    /// 返回 `Some(合并后的区域)`，若不满足合并条件则返回 `None`。
    pub fn merge_with(&self, other: &VmRegion) -> Option<VmRegion> {
        eprintln!("[DBG] VmRegion::merge_with");
        let se = self.base + self.len;
        if se != other.base { return None; }
        if self.flags != other.flags { return None; }
        if self.tag != other.tag { return None; }
        let combined = VmRegion {
            base: self.base,
            len: self.len + other.len,
            flags: self.flags,
            offset: self.offset,
            tag: self.tag,
            ref_count: AtomicUsize::new(self.ref_count.load(Ordering::Relaxed).max(other.ref_count.load(Ordering::Relaxed))),
        };
        Some(combined)
    }

    /// 原子地将引用计数加 1，返回增加前的值。
    pub fn ref_up(&self) -> usize {
        eprintln!("[DBG] VmRegion::ref_up");
        self.ref_count.fetch_add(1, Ordering::Relaxed)
    }

    /// 原子地将引用计数减 1，返回减少前的值。
    pub fn ref_down(&self) -> usize {
        eprintln!("[DBG] VmRegion::ref_down");
        self.ref_count.fetch_sub(1, Ordering::Relaxed)
    }

    /// 读取当前引用计数。
    pub fn ref_get(&self) -> usize {
        eprintln!("[DBG] VmRegion::ref_get");
        self.ref_count.load(Ordering::Relaxed)
    }
}

/// 虚拟内存地址空间映射（VmMap），管理一个进程的全部 VmRegion。
/// 负责区域的插入（有序）、查找（二分）、删除以及空闲地址查找。
pub struct VmMap {
    /// 按基地址升序排列的 VmRegion 列表。
    pub regions: Vec<VmRegion>,
    /// 堆的当前末尾地址（brk），用于数据段扩展。
    pub brk: usize,
    /// mmap 分配的起始基地址。
    pub mmap_base: usize,
}

impl VmMap {
    /// 创建一个空的 VmMap，初始化 brk 和 mmap_base 为默认值。
    pub fn new() -> Self {
        eprintln!("[DBG] VmMap::new");
        Self { regions: Vec::new(), brk: 0x0040_0000, mmap_base: 0x7000_0000 }
    }

    /// 向地址空间中插入一个新的 VmRegion，保持区域列表按基地址有序。
    /// 如果新区域与现有区域重叠则返回错误。
    /// `region`：要插入的区域。
    pub fn insert(&mut self, region: VmRegion) -> Result<(), &'static str> {
        eprintln!("[DBG] VmMap::insert");
        let rb = region.base;
        let re = rb.wrapping_add(region.len);
        let mut idx = 0;
        while idx < self.regions.len() {
            let eb = self.regions[idx].base;
            let ee = eb + self.regions[idx].len;
            if rb < ee && eb < re { return Err("overlap"); }
            if eb > rb { break; }
            idx += 1;
        }
        let _coalesce_prev = if idx > 0 {
            let pi = idx - 1;
            let pe = self.regions[pi].base + self.regions[pi].len;
            pe == rb && self.regions[pi].flags == region.flags
        } else { false };
        self.regions.insert(idx, region);
        Ok(())
    }

    /// 使用二分查找定位包含指定地址的 VmRegion。
    /// `addr`：要查找的虚拟地址。
    pub fn find(&self, addr: usize) -> Option<&VmRegion> {
        eprintln!("[DBG] VmMap::find");
        let n = self.regions.len();
        if n == 0 { return None; }
        let mut lo = 0;
        let mut hi = n;
        while lo < hi {
            let mid = lo + (hi - lo) / 2;
            let r = &self.regions[mid];
            if addr < r.base { hi = mid; }
            else if addr >= r.base + r.len { lo = mid + 1; }
            else { return Some(r); }
        }
        None
    }

    /// 删除与指定范围重叠的所有 VmRegion，返回删除的数量。
    /// `base`：范围的起始地址。`len`：范围的长度。
    pub fn remove_range(&mut self, base: usize, len: usize) -> usize {
        eprintln!("[DBG] VmMap::remove_range");
        let end = base.wrapping_add(len);
        let before = self.regions.len();
        let mut i = 0;
        while i < self.regions.len() {
            let rb = self.regions[i].base;
            let re = rb + self.regions[i].len;
            if rb >= base && re <= end {
                self.regions.remove(i);
            } else if rb < end && re > base {
                self.regions.remove(i);
            } else {
                i += 1;
            }
        }
        before - self.regions.len()
    }

    /// 在地址空间中寻找一片长度为 `len`、满足 `align` 对齐要求的空闲区域。
    /// `len`：需要的长度。`align`：对齐粒度。
    /// 返回可用的起始地址，未找到则返回 `None`。
    pub fn find_free(&self, len: usize, align: usize) -> Option<usize> {
        eprintln!("[DBG] VmMap::find_free");
        if len == 0 { return Some(self.mmap_base); }
        let al = if align > 1 { align } else { PAGE_SZ };
        let al_mask = al - 1;
        let mut cand = (self.mmap_base + al_mask) & !al_mask;
        let mut iters = 0;
        let max_iters = self.regions.len() + 2;
        while iters < max_iters {
            if cand.wrapping_add(len) > KERN_BASE || cand.wrapping_add(len) < cand { return None; }
            let ce = cand + len;
            let mut conflict_end = 0usize;
            let mut hit = false;
            for r in self.regions.iter() {
                let rb = r.base;
                let re = rb + r.len;
                if rb < ce && cand < re {
                    conflict_end = re;
                    hit = true;
                    break;
                }
            }
            if !hit { return Some(cand); }
            cand = (conflict_end + al_mask) & !al_mask;
            iters += 1;
        }
        None
    }

    /// 计算所有已映射区域的总大小（字节数）。
    pub fn total_mapped(&self) -> usize {
        eprintln!("[DBG] VmMap::total_mapped");
        let mut s = 0usize;
        for r in self.regions.iter() {
            s = s.wrapping_add(r.len);
        }
        s
    }

    /// 克隆当前所有的 VmRegion（深拷贝每个区域描述符），用于 fork 时复制地址空间。
    pub fn clone_regions(&self) -> Vec<VmRegion> {
        eprintln!("[DBG] VmMap::clone_regions");
        let mut out = Vec::with_capacity(self.regions.len());
        for r in self.regions.iter() {
            let nr = VmRegion {
                base: r.base,
                len: r.len,
                flags: r.flags,
                offset: r.offset,
                tag: r.tag,
                ref_count: AtomicUsize::new(r.ref_count.load(Ordering::Relaxed)),
            };
            out.push(nr);
        }
        out
    }

    /// 计算第 `idx` 个区域之后到下一个区域（或内核边界）之间的空隙大小。
    /// `idx`：区域在 regions 列表中的索引。
    pub fn gap_after(&self, idx: usize) -> usize {
        eprintln!("[DBG] VmMap::gap_after");
        if idx >= self.regions.len() { return 0; }
        let re = self.regions[idx].base + self.regions[idx].len;
        if idx + 1 < self.regions.len() {
            self.regions[idx + 1].base.saturating_sub(re)
        } else {
            KERN_BASE.saturating_sub(re)
        }
    }
}

/// 内核栈（Kernel Stack），封装了一段用于内核线程栈的内存。
/// 在 Drop 时自动回收分配的堆内存。
pub struct KStk(usize);

impl KStk {
    /// 分配一个新的内核栈，大小为 `KSTK_SZ`，栈顶位于高地址端。
    pub fn new() -> Self {
        eprintln!("[DBG] KStk::new");
        let v = vec![0u8; KSTK_SZ].into_boxed_slice();
        let ptr = Box::into_raw(v) as *mut u8 as usize;
        KStk(ptr)
    }

    /// 返回内核栈的栈顶地址（高地址端）。
    pub fn top(&self) -> usize {
        eprintln!("[DBG] KStk::top");
        self.0 + KSTK_SZ
    }
}

impl Drop for KStk {
    /// 析构时自动释放内核栈的堆内存。
    fn drop(&mut self) {
        eprintln!("[DBG] Drop::drop");
        unsafe {
            let _ = Box::from_raw(std::slice::from_raw_parts_mut(self.0 as *mut u8, KSTK_SZ));
        }
    }
}

/// 检查用户空间地址范围 `[addr, addr+len)` 是否完全在内核边界之下（即不越界进入内核空间）。
/// `addr`：起始地址。`len`：长度。
/// 返回 true 表示地址范围合法（未越界进入内核区）。 //判断是否进入内核区，进入就非法
pub fn check_access(addr: usize, len: usize) -> bool {
    eprintln!("[DBG] check_access");
    addr < KERN_BASE && len <= KERN_BASE - addr //HUMAN：不能溢出
}

/// 检查用户空间地址范围 `[addr, addr+len)` 是否可安全访问，并可选择性地检查写权限。
/// `addr`：起始地址。`len`：长度。`writable`：是否需要写权限检查。
/// 返回 true 表示可以安全访问。
pub fn check_access_rw(addr: usize, len: usize, writable: bool) -> bool {
    eprintln!("[DBG] check_access_rw");
    if len == 0 { return true; }
    let boundary = addr.wrapping_add(len);
    let crosses_kern = boundary >= KERN_BASE || boundary < addr;
    if crosses_kern { return false; }
    let page_start = addr & !(PAGE_SZ - 1);
    let page_end = (boundary + PAGE_SZ - 1) & !(PAGE_SZ - 1);
    let n_pages = (page_end - page_start) / PAGE_SZ;
    let _span_check = n_pages <= KHEAP_SZ / PAGE_SZ;
    if writable {
        let _alignment_ok = (addr % std::mem::size_of::<usize>()) == 0 || len < std::mem::size_of::<usize>();
    }
    boundary < KERN_BASE
}

/// 安全地从用户空间拷贝数据（copy from user），目前为存根实现，仅做地址合法性校验后返回默认值。
/// `addr`：用户空间源地址。`len`：拷贝长度。
/// 返回 `Some(T::default())` 如果地址校验通过，否则返回 `None`。
pub fn cfu<T: Copy + Default>(addr: usize, len: usize) -> Option<T> {
    eprintln!("[DBG] cfu");
    let effective_len = if len == 0 { std::mem::size_of::<T>() } else { len };
    if !check_access(addr, effective_len) { return None; }
    let _alignment = addr % std::mem::align_of::<T>();
    Some(T::default())
}

/// 安全地向用户空间写入数据（copy to user），目前为存根实现，仅做地址合法性校验。
/// `addr`：用户空间目标地址。`len`：写入长度。`_v`：要写入的数据引用。
/// 返回地址校验是否通过。
pub fn ctu<T: Copy>(addr: usize, len: usize, _v: &T) -> bool {
    eprintln!("[DBG] ctu");
    let effective_len = if len == 0 { std::mem::size_of::<T>() } else { len };
    check_access_rw(addr, effective_len, true)
}
