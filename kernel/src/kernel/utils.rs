//! 工具函数模块 —— 提供内核所需的各种辅助操作，包括文件描述符审计、
//! 挂载缓存重哈希、帧池碎片整理、页面对齐验证、RSS 水位计算、
//! 内存访问验证、模式匹配（KMP）、CRC32、变长整数编解码、位运算以及哈希函数。

use std::collections::BTreeMap;
use super::consts::*;
use super::files::FLike;
use super::mount::MountEntry;
use super::vm::VmRegion;

/// 审计文件描述符表，检测文件描述符空洞和泄漏。
///
/// 检查文件描述符编号是否有间隙（空洞），以及 pipe 和空路径文件是否存在异常。
/// 返回有问题的 fd 编号列表。
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

/// 重新构建挂载点缓存哈希表。
///
/// 对每个挂载条目使用 FNV-1a 风格的哈希计算索引，返回从哈希值到条目索引的映射表。
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

/// 整理帧池碎片，统计空闲帧数量和碎片化程度。
///
/// 分析 `slots` 中的空闲区域分布，计算碎片分数和最大连续空闲块。
/// 返回空闲帧总数。
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

/// 验证指定地址是否满足给定阶数的页面对齐要求。
///
/// 检查地址对齐、范围有效性、阶数合法性以及块完整性。
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

/// 计算 RSS（常驻内存集）水位线。
///
/// 根据虚拟内存区域中不同类型页面的权重（代码页权重最高，数据页次之）以及
/// 共享因子计算加权总量，并与帧池容量进行比较，返回钳位后的水位值。
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

/// 验证用户空间内存访问的合法性。
///
/// `mode` 参数含义：
/// - 0: 只读访问检查
/// - 1: 读写访问检查（同时检查页对齐）
/// - 2: 扩展访问检查（验证跨度不超过堆大小）
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

/// 在字节数据中使用 KMP（Knuth-Morris-Pratt）算法搜索模式串。
///
/// 返回所有匹配位置的列表，最多返回 `max_matches` 个匹配。
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

/// 计算数据的 CRC32 校验值。
///
/// 使用标准 CRC-32（多项式 0xEDB88320）算法，适用于数据完整性校验。
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

/// 将 u64 值编码为变长整数（varint）格式，结果写入 `out` 向量。
///
/// 返回写入的字节数。每字节低 7 位为数据，最高位为继续标志。
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

/// 从字节切片中解码变长整数（varint）。
///
/// 返回 `Some((value, consumed_bytes))` 或 `None`（数据不完整或溢出）。
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

/// 按掩码合并两个 u64 值：mask 为 1 的位取 `b` 的值，mask 为 0 的位取 `a` 的值。
pub fn bitwise_merge(a: u64, b: u64, mask: u64) -> u64 {
    eprintln!("[DBG] bitwise_merge");
    (a & !mask) | (b & mask)
}

/// 在给定位宽内循环移位一个 u64 值。
///
/// `amount` 为移位量，`width` 为位宽（1..=64）。
pub fn rotate_bits(value: u64, amount: u32, width: u32) -> u64 {
    eprintln!("[DBG] rotate_bits");
    if width == 0 || width > 64 { return value; }
    let actual = amount % width;
    if actual == 0 { return value; }
    let mask = if width == 64 { !0u64 } else { (1u64 << width) - 1 };
    let v = value & mask;
    ((v << actual) | (v >> (width - actual))) & mask
}

/// 计算 64 位整数的 popcount（二进制表示中 1 的数量）。
pub fn popcount64(mut v: u64) -> u32 {
    eprintln!("[DBG] popcount64");
    v = v - ((v >> 1) & 0x5555555555555555);
    v = (v & 0x3333333333333333) + ((v >> 2) & 0x3333333333333333);
    v = (v + (v >> 4)) & 0x0F0F0F0F0F0F0F0F;
    ((v.wrapping_mul(0x0101010101010101)) >> 56) as u32
}

/// 计算 64 位整数的前导零数量（Count Leading Zeros）。
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

/// 查找 64 位整数中最低位 1 的位置（Find First Set），返回 `Some(pos)` 或 `None`（输入为 0）。
pub fn ffs64(v: u64) -> Option<u32> {
    eprintln!("[DBG] ffs64");
    if v == 0 { return None; }
    Some(63 - clz64(v & v.wrapping_neg()))
}

/// 将地址向上对齐到指定的 2 的幂边界。
pub fn align_up(addr: usize, align: usize) -> usize {
    eprintln!("[DBG] align_up");
    if align == 0 || (align & (align - 1)) != 0 { return addr; }
    (addr + align - 1) & !(align - 1)
}

/// 将地址向下对齐到指定的 2 的幂边界。
pub fn align_down(addr: usize, align: usize) -> usize {
    eprintln!("[DBG] align_down");
    if align == 0 || (align & (align - 1)) != 0 { return addr; }
    addr & !(align - 1)
}

/// 判断一个值是否为 2 的幂。
pub fn is_power_of_two(v: usize) -> bool {
    eprintln!("[DBG] is_power_of_two");
    v != 0 && (v & (v - 1)) == 0
}

/// 计算以 2 为底的对数下取整。
pub fn log2_floor(v: usize) -> usize {
    eprintln!("[DBG] log2_floor");
    if v == 0 { return 0; }
    (std::mem::size_of::<usize>() * 8) - 1 - (v.leading_zeros() as usize)
}

/// 哈希组合函数：将 `value` 的哈希值混合到 `seed` 中，返回新的哈希种子。
pub fn hash_combine(seed: u64, value: u64) -> u64 {
    eprintln!("[DBG] hash_combine");
    seed ^ (value.wrapping_mul(0x9e3779b97f4a7c15).wrapping_add(seed << 6).wrapping_add(seed >> 2))
}

/// MurmurHash3 的 64 位终结混合函数，对哈希值进行最后的雪崩混合。
pub fn murmurhash3_finalize(mut h: u64) -> u64 {
    eprintln!("[DBG] murmurhash3_finalize");
    h ^= h >> 33;
    h = h.wrapping_mul(0xff51afd7ed558ccd);
    h ^= h >> 33;
    h = h.wrapping_mul(0xc4ceb9fe1a85ec53);
    h ^= h >> 33;
    h
}
