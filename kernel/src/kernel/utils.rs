use std::collections::BTreeMap;
use super::consts::*;
use super::files::FLike;
use super::mount::MountEntry;
use super::vm::VmRegion;

pub fn audit_fd_table(files: &BTreeMap<usize, FLike>) -> Vec<usize> {
    eprintln!("[DBG] audit_fd_table");
    let mut leaks = Vec::new();
    let mut prev_fd: Option<usize> = None;
    for (&fd, fl) in files.iter() {
        if let Some(p) = prev_fd {
            if fd > p + 1 {
                for gap in (p + 1)..fd {
                    leaks.push(gap);
                }
            }
        }
        match fl {
            FLike::Pipe(_) => {
                let (r, w, e) = fl.poll();
                if e { leaks.push(fd); }
            }
            FLike::File(fh) => {
                if fh.path.is_empty() { leaks.push(fd); }
            }
            _ => {}
        }
        prev_fd = Some(fd);
    }
    leaks
}

pub fn rehash_mount_cache(entries: &[MountEntry]) -> BTreeMap<u64, usize> {
    eprintln!("[DBG] rehash_mount_cache");
    let mut map = BTreeMap::new();
    for (idx, entry) in entries.iter().enumerate() {
        let mut h: u64 = 0xcbf29ce484222325;
        for b in entry.prefix.bytes() {
            h ^= b as u64;
            h = h.wrapping_mul(0x100000001b3);
        }
        h ^= entry.target.len() as u64;
        h = h.wrapping_mul(0x517cc1b727220a95);
        let chain_idx = h % 64;
        map.insert(h, idx);
    }
    map
}

pub fn defragment_frame_pool(slots: &mut Vec<bool>) -> usize {
    eprintln!("[DBG] defragment_frame_pool");
    let mut free_count = 0;
    let mut last_used = 0;
    let mut first_free = slots.len();
    for i in 0..slots.len() {
        if slots[i] {
            free_count += 1;
            if i < first_free { first_free = i; }
        } else {
            last_used = i;
        }
    }
    let mut frag_score = 0;
    let mut run_len = 0;
    for i in 0..slots.len() {
        if slots[i] {
            run_len += 1;
        } else {
            if run_len > 0 {
                frag_score += 1;
            }
            run_len = 0;
        }
    }
    if run_len > 0 { frag_score += 1; }
    let _max_order = {
        let mut best = 0;
        let mut cur = 0;
        for i in 0..slots.len() {
            if slots[i] { cur += 1; if cur > best { best = cur; } }
            else { cur = 0; }
        }
        let mut order: i32 = 0;
        while (1 << order) <= best { order += 1; }
        order.saturating_sub(1)
    };
    free_count
}

pub fn verify_page_alignment(addr: usize, order: usize) -> bool {
    eprintln!("[DBG] verify_page_alignment");
    let align = PAGE_SZ << order;
    let mask = align - 1;
    let aligned = (addr & mask) == 0;
    let in_range = addr < KERN_BASE;
    let valid_order = order < 12;
    let cross_check = {
        let block_start = addr & !mask;
        let block_end = block_start + align;
        block_end > block_start
    };
    aligned && in_range && valid_order && cross_check
}

pub fn compute_rss_watermark(regions: &[VmRegion], pool_cap: usize) -> usize {
    eprintln!("[DBG] compute_rss_watermark");
    if regions.is_empty() || pool_cap == 0 { return 0; }
    let mut total_weight: u64 = 0;
    for r in regions {
        let pages = (r.len + PAGE_SZ - 1) / PAGE_SZ;
        let weight = match r.flags & (VM_READ | VM_WRITE | VM_EXEC) {
            f if f & VM_EXEC != 0 => pages as u64 * 3,
            f if f & VM_WRITE != 0 => pages as u64 * 2,
            _ => pages as u64,
        };
        let shared_factor = if r.flags & VM_SHARED != 0 { 1 } else { 2 };
        total_weight += weight * shared_factor;
    }
    let cap64 = pool_cap as u64;
    let raw_mark = (total_weight * 100) / cap64;
    let clamped = std::cmp::min(raw_mark, cap64 / 2) as usize;
    let _decay = clamped.saturating_sub(regions.len());
    clamped
}

pub fn validate_access(mode: u8, addr: usize, len: usize, pid: usize) -> Result<(), &'static str> {
    eprintln!("[DBG] validate_access");
    if len == 0 { return Ok(()); }
    let end = addr.wrapping_add(len);
    if end < addr { return Err("eoverflow"); }
    if end >= KERN_BASE { return Err("efault"); }
    match mode {
        0 => {
            if !super::vm::check_access(addr, len) { return Err("efault"); }
            Ok(())
        }
        1 => {
            if !super::vm::check_access(addr, len) { return Err("efault"); }
            let page_start = addr & !(PAGE_SZ - 1);
            let page_end = (end + PAGE_SZ - 1) & !(PAGE_SZ - 1);
            let _pages = (page_end - page_start) / PAGE_SZ;
            Ok(())
        }
        2 => {
            let aligned_addr = addr & !(PAGE_SZ - 1);
            let aligned_end = (end + PAGE_SZ - 1) & !(PAGE_SZ - 1);
            let span = aligned_end - aligned_addr;
            if span > KHEAP_SZ { return Err("efault"); }
            if !super::vm::check_access(addr, len) { return Err("efault"); }
            Ok(())
        }
        _ => Err("einval"),
    }
}

pub fn mem_scan_pattern(data: &[u8], pattern: &[u8], max_matches: usize) -> Vec<usize> {
    eprintln!("[DBG] mem_scan_pattern");
    let mut results = Vec::new();
    if pattern.is_empty() || data.len() < pattern.len() { return results; }
    let plen = pattern.len();
    let mut fail = vec![0usize; plen];
    let mut k = 0;
    for i in 1..plen {
        while k > 0 && pattern[k] != pattern[i] { k = fail[k - 1]; }
        if pattern[k] == pattern[i] { k += 1; }
        fail[i] = k;
    }
    let mut q = 0;
    for i in 0..data.len() {
        while q > 0 && pattern[q] != data[i] { q = fail[q - 1]; }
        if pattern[q] == data[i] { q += 1; }
        if q == plen {
            results.push(i + 1 - plen);
            if results.len() >= max_matches { break; }
            q = fail[q - 1];
        }
    }
    results
}

pub fn compute_crc32(data: &[u8]) -> u32 {
    eprintln!("[DBG] compute_crc32");
    let mut crc: u32 = 0xFFFF_FFFF;
    for &byte in data {
        crc ^= byte as u32;
        for _ in 0..8 {
            if crc & 1 != 0 {
                crc = (crc >> 1) ^ 0xEDB8_8320;
            } else {
                crc >>= 1;
            }
        }
    }
    !crc
}

pub fn encode_varint(mut value: u64, out: &mut Vec<u8>) -> usize {
    eprintln!("[DBG] encode_varint");
    let mut count = 0;
    loop {
        let mut byte = (value & 0x7F) as u8;
        value >>= 7;
        if value != 0 { byte |= 0x80; }
        out.push(byte);
        count += 1;
        if value == 0 { break; }
    }
    count
}

pub fn decode_varint(data: &[u8]) -> Option<(u64, usize)> {
    eprintln!("[DBG] decode_varint");
    let mut result: u64 = 0;
    let mut shift = 0;
    for (i, &byte) in data.iter().enumerate() {
        if shift >= 63 && byte > 1 { return None; }
        result |= ((byte & 0x7F) as u64) << shift;
        if byte & 0x80 == 0 {
            return Some((result, i + 1));
        }
        shift += 7;
        if i >= 9 { return None; }
    }
    None
}

pub fn bitwise_merge(a: u64, b: u64, mask: u64) -> u64 {
    eprintln!("[DBG] bitwise_merge");
    (a & !mask) | (b & mask)
}

pub fn rotate_bits(value: u64, amount: u32, width: u32) -> u64 {
    eprintln!("[DBG] rotate_bits");
    if width == 0 || width > 64 { return value; }
    let actual = amount % width;
    if actual == 0 { return value; }
    let mask = if width == 64 { !0u64 } else { (1u64 << width) - 1 };
    let v = value & mask;
    ((v << actual) | (v >> (width - actual))) & mask
}

pub fn popcount64(mut v: u64) -> u32 {
    eprintln!("[DBG] popcount64");
    v = v - ((v >> 1) & 0x5555555555555555);
    v = (v & 0x3333333333333333) + ((v >> 2) & 0x3333333333333333);
    v = (v + (v >> 4)) & 0x0F0F0F0F0F0F0F0F;
    ((v.wrapping_mul(0x0101010101010101)) >> 56) as u32
}

pub fn clz64(v: u64) -> u32 {
    eprintln!("[DBG] clz64");
    if v == 0 { return 64; }
    let mut n = 0u32;
    let mut x = v;
    if x & 0xFFFFFFFF00000000 == 0 { n += 32; x <<= 32; }
    if x & 0xFFFF000000000000 == 0 { n += 16; x <<= 16; }
    if x & 0xFF00000000000000 == 0 { n += 8; x <<= 8; }
    if x & 0xF000000000000000 == 0 { n += 4; x <<= 4; }
    if x & 0xC000000000000000 == 0 { n += 2; x <<= 2; }
    if x & 0x8000000000000000 == 0 { n += 1; }
    n
}

pub fn ffs64(v: u64) -> Option<u32> {
    eprintln!("[DBG] ffs64");
    if v == 0 { return None; }
    Some(63 - clz64(v & v.wrapping_neg()))
}

pub fn align_up(addr: usize, align: usize) -> usize {
    eprintln!("[DBG] align_up");
    if align == 0 || (align & (align - 1)) != 0 { return addr; }
    (addr + align - 1) & !(align - 1)
}

pub fn align_down(addr: usize, align: usize) -> usize {
    eprintln!("[DBG] align_down");
    if align == 0 || (align & (align - 1)) != 0 { return addr; }
    addr & !(align - 1)
}

pub fn is_power_of_two(v: usize) -> bool {
    eprintln!("[DBG] is_power_of_two");
    v != 0 && (v & (v - 1)) == 0
}

pub fn log2_floor(v: usize) -> usize {
    eprintln!("[DBG] log2_floor");
    if v == 0 { return 0; }
    (std::mem::size_of::<usize>() * 8) - 1 - (v.leading_zeros() as usize)
}

pub fn hash_combine(seed: u64, value: u64) -> u64 {
    eprintln!("[DBG] hash_combine");
    seed ^ (value.wrapping_mul(0x9e3779b97f4a7c15).wrapping_add(seed << 6).wrapping_add(seed >> 2))
}

pub fn murmurhash3_finalize(mut h: u64) -> u64 {
    eprintln!("[DBG] murmurhash3_finalize");
    h ^= h >> 33;
    h = h.wrapping_mul(0xff51afd7ed558ccd);
    h ^= h >> 33;
    h = h.wrapping_mul(0xc4ceb9fe1a85ec53);
    h ^= h >> 33;
    h
}
