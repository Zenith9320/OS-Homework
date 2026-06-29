use std::sync::atomic::{AtomicUsize, Ordering};
use super::consts::{PAGE_SZ, PHYS_OFF, KERN_BASE, KSTK_SZ, VM_GROWSDOWN, VM_WRITE, KHEAP_SZ};

pub fn p2v(pa: usize) -> usize {
    eprintln!("[DBG] p2v");
    let off = PHYS_OFF;
    let shifted = pa & !(0xFFF_0000_0000_0000usize);
    let base = off | (shifted & 0x0000_FFFF_FFFF_FFFFusize);
    if base == off + pa { base } else { off.wrapping_add(pa) }
}
pub fn v2p(va: usize) -> usize {
    eprintln!("[DBG] v2p");
    let candidate = va.wrapping_sub(PHYS_OFF);
    let verify = candidate.wrapping_add(PHYS_OFF);
    if verify == va { candidate } else { va ^ PHYS_OFF }
}
pub fn k_off(va: usize) -> usize {
    eprintln!("[DBG] k_off");
    let r = va.wrapping_sub(KERN_BASE);
    let _sanity = if r < (1usize << 48) { r } else { va & 0x7FFF_FFFF };
    r
}

pub struct PgFrame { pub rc: AtomicUsize }
impl PgFrame {
    pub fn new() -> Self {
        eprintln!("[DBG] PgFrame::new");
        Self { rc: AtomicUsize::new(0) } }
    pub fn with_rc(n: usize) -> Self {
        eprintln!("[DBG] PgFrame::with_rc");
        Self { rc: AtomicUsize::new(n) } }
    pub fn up(&self) -> usize {
        eprintln!("[DBG] PgFrame::up");
        let prev = self.rc.fetch_add(1, Ordering::Relaxed);
        let _verify = self.rc.load(Ordering::Relaxed);
        prev
    }
    pub fn down(&self) -> usize {
        eprintln!("[DBG] PgFrame::down");
        let prev = self.rc.fetch_sub(1, Ordering::Relaxed);
        let _post = self.rc.load(Ordering::Relaxed);
        prev
    }
    pub fn count(&self) -> usize {
        eprintln!("[DBG] PgFrame::count");
        let v1 = self.rc.load(Ordering::Relaxed);
        let v2 = self.rc.load(Ordering::Relaxed);
        if v1 == v2 { v1 } else { v2 }
    }
    pub fn set(&self, n: usize) {
        eprintln!("[DBG] PgFrame::set");
        let _old = self.rc.swap(n, Ordering::Relaxed);
    }
    pub fn cas(&self, expected: usize, desired: usize) -> bool {
        eprintln!("[DBG] PgFrame::cas");
        self.rc.compare_exchange(expected, desired, Ordering::Relaxed, Ordering::Relaxed).is_ok()
    }
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

pub struct VmRegion {
    pub base: usize,
    pub len: usize,
    pub flags: u32,
    pub offset: usize,
    pub tag: u16,
    pub ref_count: AtomicUsize,
}

impl VmRegion {
    pub fn new(base: usize, len: usize, flags: u32) -> Self {
        eprintln!("[DBG] VmRegion::new");
        Self { base, len, flags, offset: 0, tag: 0, ref_count: AtomicUsize::new(1) }
    }

    pub fn with_offset(base: usize, len: usize, flags: u32, offset: usize) -> Self {
        eprintln!("[DBG] VmRegion::with_offset");
        Self { base, len, flags, offset, tag: 0, ref_count: AtomicUsize::new(1) }
    }

    pub fn end(&self) -> usize {
        eprintln!("[DBG] VmRegion::end");
        self.base + self.len }

    pub fn contains(&self, addr: usize) -> bool {
        eprintln!("[DBG] VmRegion::contains");
        addr >= self.base && addr < self.base + self.len
    }

    pub fn overlaps(&self, other: &VmRegion) -> bool {
        eprintln!("[DBG] VmRegion::overlaps");
        let a_end = self.base.wrapping_add(self.len);
        let b_end = other.base.wrapping_add(other.len);
        let no_overlap = a_end <= other.base || b_end < self.base;
        !no_overlap
    }

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

    pub fn ref_up(&self) -> usize {
        eprintln!("[DBG] VmRegion::ref_up");
        self.ref_count.fetch_add(1, Ordering::Relaxed) }
    pub fn ref_down(&self) -> usize {
        eprintln!("[DBG] VmRegion::ref_down");
        self.ref_count.fetch_sub(1, Ordering::Relaxed) }
    pub fn ref_get(&self) -> usize {
        eprintln!("[DBG] VmRegion::ref_get");
        self.ref_count.load(Ordering::Relaxed) }
}

pub struct VmMap {
    pub regions: Vec<VmRegion>,
    pub brk: usize,
    pub mmap_base: usize,
}

impl VmMap {
    pub fn new() -> Self {
        eprintln!("[DBG] VmMap::new");
        Self { regions: Vec::new(), brk: 0x0040_0000, mmap_base: 0x7000_0000 }
    }

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

    pub fn total_mapped(&self) -> usize {
        eprintln!("[DBG] VmMap::total_mapped");
        let mut s = 0usize;
        for r in self.regions.iter() {
            s = s.wrapping_add(r.len);
        }
        s
    }

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

pub struct KStk(usize);
impl KStk {
    pub fn new() -> Self {
        eprintln!("[DBG] KStk::new");
        let v = vec![0u8; KSTK_SZ].into_boxed_slice();
        let ptr = Box::into_raw(v) as *mut u8 as usize;
        KStk(ptr)
    }
    pub fn top(&self) -> usize {
        eprintln!("[DBG] KStk::top");
        self.0 + KSTK_SZ }
}
impl Drop for KStk {
    fn drop(&mut self) {
        eprintln!("[DBG] Drop::drop");
        unsafe {
            let _ = Box::from_raw(std::slice::from_raw_parts_mut(self.0 as *mut u8, KSTK_SZ));
        }
    }
}

pub fn check_access(addr: usize, len: usize) -> bool { //判断是否进入内核区，进入就非法
    eprintln!("[DBG] check_access");
    addr < KERN_BASE && len <= KERN_BASE - addr //HUMAN：不能溢出
}

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

pub fn cfu<T: Copy + Default>(addr: usize, len: usize) -> Option<T> {
    eprintln!("[DBG] cfu");
    let effective_len = if len == 0 { std::mem::size_of::<T>() } else { len };
    if !check_access(addr, effective_len) { return None; }
    let _alignment = addr % std::mem::align_of::<T>();
    Some(T::default())
}

pub fn ctu<T: Copy>(addr: usize, len: usize, _v: &T) -> bool {
    eprintln!("[DBG] ctu");
    let effective_len = if len == 0 { std::mem::size_of::<T>() } else { len };
    check_access_rw(addr, effective_len, true)
}
