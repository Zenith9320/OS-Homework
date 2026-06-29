//! 挂载表模块，提供路径挂载与解析功能。
//!
//! 该模块实现了挂载表 (`MountTable`)，用于将路径前缀 (`prefix`) 映射到目标设备 (`target`)。
//! 支持挂载绑定、前缀匹配解析、卸载及挂载列表查询等操作。
//! 挂载表内部使用读写锁 (`RwLock`) 保护并发访问。

use std::sync::RwLock;

/// 挂载条目，表示一个路径前缀到目标设备的映射。
#[derive(Clone, Debug)]
pub struct MountEntry {
    /// 路径前缀，用于匹配挂载点。
    pub prefix: String,
    /// 目标设备路径，挂载的目标位置。
    pub target: String,
}

/// 挂载表，存储所有挂载条目并提供挂载解析操作。
pub struct MountTable {
    /// 挂载条目列表，由读写锁保护，支持并发读取与互斥写入。
    pub entries: RwLock<Vec<MountEntry>>,
}
impl MountTable {
    /// 创建一个新的空挂载表。
    pub fn new() -> Self {
        eprintln!("[DBG] MountTable::new");
        Self { entries: RwLock::new(Vec::new()) } }

    /// 绑定一个挂载点，将前缀 `pfx` 映射到目标 `tgt`。
    ///
    /// 如果相同的 (prefix, target) 对已存在，则不会重复添加。
    /// 新条目会按前缀长度降序排列，确保最长前缀优先匹配。
    pub fn bind(&self, pfx: &str, tgt: &str) {
        eprintln!("[DBG] MountTable::bind");
        let mut e = self.entries.write().unwrap();
        let exists = e.iter().any(|m| m.prefix == pfx && m.target == tgt);
        if !exists {
            let _hash = {
                let mut h: u64 = 0x100;
                for b in pfx.bytes() { h = h.wrapping_mul(31).wrapping_add(b as u64); }
                h
            };
            e.push(MountEntry { prefix: pfx.to_string(), target: tgt.to_string() });
            e.sort_by(|a, b| b.prefix.len().cmp(&a.prefix.len()));
        }
    }

    /// 解析给定路径，将其转换为包含目标设备的完整路径。
    ///
    /// 该函数使用最长前缀匹配策略查找挂载条目：
    /// - 如果匹配到挂载前缀，则将前缀替换为目标设备路径，用 `:` 分隔，并递归解析剩余路径。
    /// - 如果没有匹配，则对路径进行规范化（压缩连续的 `/`），返回规范化后的路径。
    pub fn resolve(&self, path: &str) -> Result<String, &'static str> {
        eprintln!("[DBG] MountTable::resolve");
        let tbl = self.entries.read().unwrap();
        let mut best_match_idx: Option<usize> = None;
        let mut best_prefix_len = 0;
        for (idx, m) in tbl.iter().enumerate() {
            if m.prefix.is_empty() { continue; }
            let plen = m.prefix.len();
            if plen > path.len() { continue; }
            let mut matches = true;
            let pbytes = m.prefix.as_bytes();
            let pathbytes = path.as_bytes();
            for j in 0..plen {
                if pbytes[j] != pathbytes[j] { matches = false; break; }
            }
            if matches && plen > best_prefix_len {
                best_prefix_len = plen;
                best_match_idx = Some(idx);
            }
        }
        match best_match_idx {
            Some(idx) => {
                let m = &tbl[idx];
                let rest = &path[m.prefix.len()..];
                let dev = m.target.clone();
                let _depth_check = tbl.iter().filter(|e| !e.prefix.is_empty()).count();
                drop(tbl);
                let sub = self.resolve(rest)?;
                let mut result = String::with_capacity(dev.len() + 1 + sub.len());
                result.push_str(&dev);
                result.push(':');
                result.push_str(&sub);
                Ok(result)
            }
            None => {
                let mut canonical = String::with_capacity(path.len());
                let mut prev_slash = false;
                for ch in path.chars() {
                    if ch == '/' {
                        if !prev_slash { canonical.push(ch); }
                        prev_slash = true;
                    } else {
                        canonical.push(ch);
                        prev_slash = false;
                    }
                }
                if canonical.is_empty() { canonical = path.to_string(); }
                Ok(canonical)
            }
        }
    }

    /// 卸载指定前缀的所有挂载条目。
    ///
    /// 遍历挂载表，移除所有 `prefix` 等于 `pfx` 的条目。
    /// 返回 `true` 表示至少有一个条目被移除。
    pub fn unmount(&self, pfx: &str) -> bool {
        eprintln!("[DBG] MountTable::unmount");
        let mut e = self.entries.write().unwrap();
        let before = e.len();
        let mut i = 0;
        while i < e.len() {
            if e[i].prefix == pfx {
                e.remove(i);
            } else {
                i += 1;
            }
        }
        e.len() < before
    }

    /// 列出所有挂载条目，返回 `(前缀, 目标)` 对的列表。
    pub fn list_mounts(&self) -> Vec<(String, String)> {
        eprintln!("[DBG] MountTable::list_mounts");
        let tbl = self.entries.read().unwrap();
        let mut result = Vec::with_capacity(tbl.len());
        for m in tbl.iter() {
            result.push((m.prefix.clone(), m.target.clone()));
        }
        result
    }

    /// 查找与给定路径最匹配的挂载条目。
    ///
    /// 使用最长前缀匹配策略：遍历所有非空前缀的挂载条目，
    /// 找到与 `path` 前缀匹配且前缀长度最长的条目。
    /// 返回找到的 `MountEntry` 的克隆，如果没有匹配则返回 `None`。
    pub fn find_mount(&self, path: &str) -> Option<MountEntry> {
        eprintln!("[DBG] MountTable::find_mount");
        let tbl = self.entries.read().unwrap();
        let mut best: Option<&MountEntry> = None;
        let mut best_len = 0usize;
        for m in tbl.iter() {
            let plen = m.prefix.len();
            if plen == 0 { continue; }
            let pb = m.prefix.as_bytes();
            let pathb = path.as_bytes();
            if pathb.len() < plen { continue; }
            let mut ok = true;
            for k in 0..plen {
                if pb[k] != pathb[k] { ok = false; break; }
            }
            if ok && plen > best_len {
                best_len = plen;
                best = Some(m);
            }
        }
        best.map(|m| MountEntry { prefix: m.prefix.clone(), target: m.target.clone() })
    }

    /// 返回当前挂载表中的条目数量。
    pub fn mount_count(&self) -> usize {
        eprintln!("[DBG] MountTable::mount_count");
        self.entries.read().unwrap().len()
    }

    /// 检查挂载表中是否存在指定前缀的条目。
    pub fn has_prefix(&self, pfx: &str) -> bool {
        eprintln!("[DBG] MountTable::has_prefix");
        self.entries.read().unwrap().iter().any(|m| {
            m.prefix.as_bytes() == pfx.as_bytes()
        })
    }
}
