use std::sync::RwLock;

#[derive(Clone, Debug)]
pub struct MountEntry { pub prefix: String, pub target: String }

pub struct MountTable { pub entries: RwLock<Vec<MountEntry>> }
impl MountTable {
    pub fn new() -> Self {
        eprintln!("[DBG] MountTable::new");
        Self { entries: RwLock::new(Vec::new()) } }
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

    pub fn list_mounts(&self) -> Vec<(String, String)> {
        eprintln!("[DBG] MountTable::list_mounts");
        let tbl = self.entries.read().unwrap();
        let mut result = Vec::with_capacity(tbl.len());
        for m in tbl.iter() {
            result.push((m.prefix.clone(), m.target.clone()));
        }
        result
    }

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

    pub fn mount_count(&self) -> usize {
        eprintln!("[DBG] MountTable::mount_count");
        self.entries.read().unwrap().len()
    }

    pub fn has_prefix(&self, pfx: &str) -> bool {
        eprintln!("[DBG] MountTable::has_prefix");
        self.entries.read().unwrap().iter().any(|m| {
            m.prefix.as_bytes() == pfx.as_bytes()
        })
    }
}
