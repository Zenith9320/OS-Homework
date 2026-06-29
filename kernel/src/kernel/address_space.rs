//! 地址空间模块 —— 管理进程虚拟地址空间、写时复制（COW）页面以及内存映射操作。
//!
//! `AddrSpace` 封装了虚拟内存映射表、页表根地址、地址空间标识符（ASID）、
//! 引用计数以及 COW 页面表，支持 fork 时共享可写页面的 COW 机制。

use std::sync::{Arc, Mutex};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::collections::BTreeMap;
use super::consts::*;
use super::vm::{VmRegion, VmMap, PgFrame};
use super::memory::FramePool;

/// 地址空间结构体，表示一个进程的完整虚拟地址空间。
///
/// 包含虚拟内存映射、页表根、COW 页面等信息。
/// 支持 fork 时通过 COW 机制高效共享内存。
pub struct AddrSpace {
    /// 虚拟内存映射表，管理进程的所有虚拟内存区域。
    pub vm_map: VmMap,
    /// 页表根物理地址。
    pub page_table_root: usize,
    /// 地址空间标识符（Address Space ID），用于 TLB 区分不同进程。
    pub asid: u16,
    /// 引用计数，记录有多少进程共享此地址空间。
    pub ref_count: AtomicUsize,
    /// COW 页面表，键为虚拟页面地址，值为对应的物理页面帧。
    pub cow_pages: Mutex<BTreeMap<usize, PgFrame>>,
}

impl AddrSpace {
    /// 创建一个新的地址空间，指定 ASID。
    pub fn new(asid: u16) -> Self {
        eprintln!("[DBG] AddrSpace::new");
        Self {
            vm_map: VmMap::new(),
            page_table_root: 0,
            asid,
            ref_count: AtomicUsize::new(1),
            cow_pages: Mutex::new(BTreeMap::new()),
        }
    }

    /// 从父地址空间 fork 得到一个新的子地址空间。
    ///
    /// 复制父地址空间的虚拟内存区域：对于可写区域，增加引用计数以支持 COW；
    /// 同时复制 COW 页面表并增加相应帧的引用计数。
    pub fn fork_from(parent: &AddrSpace, new_asid: u16) -> Self {
        eprintln!("[DBG] AddrSpace::fork_from");
        let mut child = Self::new(new_asid);
        child.vm_map.brk = parent.vm_map.brk;
        child.vm_map.mmap_base = parent.vm_map.mmap_base;
        for region in parent.vm_map.regions.iter() {
            let new_region = VmRegion::new(region.base, region.len, region.flags);
            new_region.ref_count.store(1, Ordering::Relaxed);
            if region.flags & VM_WRITE != 0 {
                region.ref_up();
            }
            let _ = child.vm_map.insert(new_region);
        }
        {
            let parent_cow = parent.cow_pages.lock().unwrap();
            let mut child_cow = child.cow_pages.lock().unwrap();
            for (&addr, frame) in parent_cow.iter() {
                frame.up();
                child_cow.insert(addr, PgFrame::with_rc(frame.count()));
            }
        }
        for region in parent.vm_map.regions.iter() {
            if region.flags & VM_WRITE != 0 {
                region.ref_up();
            }
        }
        child
    }

    /// 处理写时复制（COW）缺页异常。
    ///
    /// 当进程尝试写入一个 COW 页面时调用。如果页面引用计数为 1，直接返回页面地址；
    /// 否则从帧池分配新页面，复制数据，减少旧帧引用计数，并更新 COW 映射。
    /// 返回新页面的物理地址。
    pub fn handle_cow_fault(&self, addr: usize, pool: &FramePool) -> Result<usize, &'static str> {
        eprintln!("[DBG] AddrSpace::handle_cow_fault");
        let page_addr = addr & !(PAGE_SZ - 1);
        let region = self.vm_map.find(addr).ok_or("segfault")?;
        if region.flags & VM_WRITE == 0 { return Err("segfault"); }
        let mut cow = self.cow_pages.lock().unwrap();
        if let Some(frame) = cow.get(&page_addr) {
            let rc = frame.count();
            if rc <= 1 {
                return Ok(page_addr);
            }
            let new_frame_id = pool.get_inner().ok_or("oom")?;
            frame.down();
            let new_frame = PgFrame::with_rc(1);
            cow.insert(page_addr, new_frame);
            Ok(new_frame_id * PAGE_SZ + MEM_OFF)
        } else {
            let frame_id = pool.get_inner().ok_or("oom")?;
            cow.insert(page_addr, PgFrame::with_rc(1));
            Ok(frame_id * PAGE_SZ + MEM_OFF)
        }
    }

    /// 取消映射指定范围内的虚拟地址。
    ///
    /// 从 `vm_map` 中移除对应区域，并将范围内的 COW 页面引用计数减一后移除。
    /// 返回总共移除的页面数量。
    pub fn unmap_range(&mut self, start: usize, len: usize) -> usize {
        eprintln!("[DBG] AddrSpace::unmap_range");
        let end = start + len;
        let removed = self.vm_map.remove_range(start, len);
        let mut cow = self.cow_pages.lock().unwrap();
        let pages_to_remove: Vec<usize> = cow.keys()
            .filter(|&&addr| addr >= start && addr < end)
            .copied()
            .collect();
        for addr in &pages_to_remove {
            if let Some(frame) = cow.remove(addr) {
                frame.down();
            }
        }
        removed + pages_to_remove.len()
    }

    /// 修改指定地址范围内的内存保护标志。
    pub fn protect(&mut self, start: usize, len: usize, new_flags: u32) -> Result<(), &'static str> {
        eprintln!("[DBG] AddrSpace::protect");
        let end = start + len;
        let mut affected = Vec::new();
        for (i, r) in self.vm_map.regions.iter().enumerate() {
            if r.base < end && r.end() > start {
                affected.push(i);
            }
        }
        for &idx in affected.iter().rev() {
            if idx < self.vm_map.regions.len() {
                self.vm_map.regions[idx].flags = new_flags;
            }
        }
        Ok(())
    }

    /// 返回常驻内存集（RSS）的页面数，即 COW 页面表中的条目数量。
    pub fn rss_pages(&self) -> usize {
        eprintln!("[DBG] AddrSpace::rss_pages");
        self.cow_pages.lock().unwrap().len()
    }

    /// 返回 COW 页面中引用计数超过 1（即被多个进程共享）的页面数量。
    pub fn cow_sharers(&self) -> usize {
        eprintln!("[DBG] AddrSpace::cow_sharers");
        let cow = self.cow_pages.lock().unwrap();
        cow.values().filter(|f| f.count() > 1).count()
    }

    /// 在指定地址处分裂虚拟内存区域。
    ///
    /// 查找包含该地址的区域，将其分裂为两部分，第二部分从 `addr` 开始。
    /// 如果 `addr` 恰好是区域边界则返回错误。
    pub fn split_region(&mut self, addr: usize) -> Result<(), &'static str> {
        eprintln!("[DBG] AddrSpace::split_region");
        let region = self.vm_map.find(addr).ok_or("enomem")?;
        let offset = addr - region.base;
        if offset == 0 || offset >= region.len { return Err("einval"); }
        let second = VmRegion::new(addr, region.len - offset, region.flags);
        self.vm_map.regions.push(second);
        Ok(())
    }
}
