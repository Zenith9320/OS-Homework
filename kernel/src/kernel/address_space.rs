use std::sync::{Arc, Mutex};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::collections::BTreeMap;
use super::consts::*;
use super::vm::{VmRegion, VmMap, PgFrame};
use super::memory::FramePool;

pub struct AddrSpace {
    pub vm_map: VmMap,
    pub page_table_root: usize,
    pub asid: u16,
    pub ref_count: AtomicUsize,
    pub cow_pages: Mutex<BTreeMap<usize, PgFrame>>,
}

impl AddrSpace {
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

    pub fn rss_pages(&self) -> usize {
        eprintln!("[DBG] AddrSpace::rss_pages");
        self.cow_pages.lock().unwrap().len()
    }

    pub fn cow_sharers(&self) -> usize {
        eprintln!("[DBG] AddrSpace::cow_sharers");
        let cow = self.cow_pages.lock().unwrap();
        cow.values().filter(|f| f.count() > 1).count()
    }

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
