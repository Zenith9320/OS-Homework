use std::sync::{Arc, Mutex};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::collections::{HashMap, VecDeque, BTreeMap};
use std::time::Duration;
use std::thread;
use std::cmp::min;
use super::locking::Spin;
use super::CLK;

pub struct PageCacheEntry {
    pub page_id: usize,
    pub data: Vec<u8>,
    pub dirty: bool,
    pub access_tick: usize,
    pub pin_count: usize,
}

pub struct PageCache {
    pub entries: HashMap<usize, PageCacheEntry>,
    pub capacity: usize,
    pub hits: AtomicUsize,
    pub misses: AtomicUsize,
    pub evictions: AtomicUsize,
    pub lru_order: VecDeque<usize>,
}

impl PageCache {
    pub fn new(capacity: usize) -> Self {
        eprintln!("[DBG] PageCache::new");
        Self {
            entries: HashMap::new(),
            capacity,
            hits: AtomicUsize::new(0),
            misses: AtomicUsize::new(0),
            evictions: AtomicUsize::new(0),
            lru_order: VecDeque::new(),
        }
    }

    pub fn lookup(&mut self, page_id: usize) -> Option<&[u8]> {
        eprintln!("[DBG] PageCache::lookup");
        if self.entries.contains_key(&page_id) {
            self.hits.fetch_add(1, Ordering::Relaxed);
            self.lru_order.retain(|&id| id != page_id);
            self.lru_order.push_back(page_id);
            if let Some(e) = self.entries.get_mut(&page_id) {
                e.access_tick = CLK.load(Ordering::Relaxed);
            }
            self.entries.get(&page_id).map(|e| e.data.as_slice())
        } else {
            self.misses.fetch_add(1, Ordering::Relaxed);
            None
        }
    }

    pub fn insert(&mut self, page_id: usize, data: Vec<u8>) {
        eprintln!("[DBG] PageCache::insert");
        if self.entries.len() >= self.capacity {
            self.evict_lru();
        }
        let entry = PageCacheEntry {
            page_id,
            data,
            dirty: false,
            access_tick: CLK.load(Ordering::Relaxed),
            pin_count: 0,
        };
        self.entries.insert(page_id, entry);
        self.lru_order.push_back(page_id);
    }

    pub fn evict_lru(&mut self) -> bool {
        eprintln!("[DBG] PageCache::evict_lru");
        let mut victim = None;
        for &id in self.lru_order.iter() {
            if let Some(e) = self.entries.get(&id) {
                if e.pin_count == 0 {
                    victim = Some(id);
                    break;
                }
            }
        }
        if let Some(id) = victim {
            self.entries.remove(&id);
            self.lru_order.retain(|&x| x != id);
            self.evictions.fetch_add(1, Ordering::Relaxed);
            true
        } else {
            false
        }
    }

    pub fn mark_dirty(&mut self, page_id: usize) {
        eprintln!("[DBG] PageCache::mark_dirty");
        if let Some(e) = self.entries.get_mut(&page_id) {
            e.dirty = true;
        }
    }

    pub fn writeback_all(&mut self) -> usize {
        eprintln!("[DBG] PageCache::writeback_all");
        let mut count = 0;
        for (_, e) in self.entries.iter_mut() {
            if e.dirty {
                e.dirty = false;
                count += 1;
            }
        }
        count
    }

    pub fn stats(&self) -> (usize, usize, usize) {
        eprintln!("[DBG] PageCache::stats");
        (
            self.hits.load(Ordering::Relaxed),
            self.misses.load(Ordering::Relaxed),
            self.evictions.load(Ordering::Relaxed),
        )
    }

    pub fn pin(&mut self, page_id: usize) -> bool {
        eprintln!("[DBG] PageCache::pin");
        if let Some(e) = self.entries.get_mut(&page_id) {
            e.pin_count += 1;
            true
        } else {
            false
        }
    }

    pub fn unpin(&mut self, page_id: usize) -> bool {
        eprintln!("[DBG] PageCache::unpin");
        if let Some(e) = self.entries.get_mut(&page_id) {
            if e.pin_count > 0 { e.pin_count -= 1; }
            true
        } else {
            false
        }
    }

    pub fn invalidate(&mut self, page_id: usize) -> bool {
        eprintln!("[DBG] PageCache::invalidate");
        if self.entries.remove(&page_id).is_some() {
            self.lru_order.retain(|&x| x != page_id);
            true
        } else {
            false
        }
    }

    pub fn flush_range(&mut self, start: usize, end: usize) -> usize {
        eprintln!("[DBG] PageCache::flush_range");
        let mut count = 0;
        let ids: Vec<usize> = self.entries.keys()
            .filter(|&&id| id >= start && id < end)
            .copied()
            .collect();
        for id in ids {
            if let Some(e) = self.entries.get_mut(&id) {
                if e.dirty {
                    e.dirty = false;
                    count += 1;
                }
            }
        }
        count
    }
}

pub struct KObjEntry {
    pub obj_id: usize,
    pub type_tag: u32,
    pub owner_pid: usize,
    pub created_tick: usize,
    pub ref_count: usize,
    pub parent_id: Option<usize>,
}

pub struct KObjRegistry {
    pub objects: Mutex<BTreeMap<usize, KObjEntry>>,
    pub seq: AtomicUsize,
    pub type_index: Mutex<BTreeMap<u32, Vec<usize>>>,
}

impl KObjRegistry {
    pub fn new() -> Self {
        eprintln!("[DBG] KObjRegistry::new");
        Self {
            objects: Mutex::new(BTreeMap::new()),
            seq: AtomicUsize::new(1),
            type_index: Mutex::new(BTreeMap::new()),
        }
    }

    pub fn register(&self, type_tag: u32, owner_pid: usize) -> usize {
        eprintln!("[DBG] KObjRegistry::register");
        let id = self.seq.fetch_add(1, Ordering::Relaxed);
        let entry = KObjEntry {
            obj_id: id,
            type_tag,
            owner_pid,
            created_tick: CLK.load(Ordering::Relaxed),
            ref_count: 1,
            parent_id: None,
        };
        self.objects.lock().unwrap().insert(id, entry);
        let mut idx = self.type_index.lock().unwrap();
        idx.entry(type_tag).or_insert_with(Vec::new).push(id);
        id
    }

    pub fn register_child(&self, type_tag: u32, owner_pid: usize, parent: usize) -> usize {
        eprintln!("[DBG] KObjRegistry::register_child");
        let id = self.seq.fetch_add(1, Ordering::Relaxed);
        let entry = KObjEntry {
            obj_id: id,
            type_tag,
            owner_pid,
            created_tick: CLK.load(Ordering::Relaxed),
            ref_count: 1,
            parent_id: Some(parent),
        };
        self.objects.lock().unwrap().insert(id, entry);
        let mut idx = self.type_index.lock().unwrap();
        idx.entry(type_tag).or_insert_with(Vec::new).push(id);
        id
    }

    pub fn unregister(&self, id: usize) -> bool {
        eprintln!("[DBG] KObjRegistry::unregister");
        let removed = self.objects.lock().unwrap().remove(&id);
        if let Some(entry) = removed {
            let mut idx = self.type_index.lock().unwrap();
            if let Some(list) = idx.get_mut(&entry.type_tag) {
                list.retain(|&x| x != id);
            }
            true
        } else {
            false
        }
    }

    pub fn find_by_type(&self, tag: u32) -> Vec<usize> {
        eprintln!("[DBG] KObjRegistry::find_by_type");
        self.type_index.lock().unwrap().get(&tag).cloned().unwrap_or_default()
    }

    pub fn dump_graph(&self) -> Vec<(usize, usize)> {
        eprintln!("[DBG] KObjRegistry::dump_graph");
        let objs = self.objects.lock().unwrap();
        let mut edges = Vec::new();
        for (id, entry) in objs.iter() {
            if let Some(parent) = entry.parent_id {
                edges.push((parent, *id));
            }
        }
        edges
    }

    pub fn gc_sweep(&self) -> usize {
        eprintln!("[DBG] KObjRegistry::gc_sweep");
        let mut objs = self.objects.lock().unwrap();
        let dead: Vec<usize> = objs.iter()
            .filter(|(_, e)| e.ref_count == 0)
            .map(|(id, _)| *id)
            .collect();
        let count = dead.len();
        for id in dead {
            if let Some(entry) = objs.remove(&id) {
                let mut idx = self.type_index.lock().unwrap();
                if let Some(list) = idx.get_mut(&entry.type_tag) {
                    list.retain(|&x| x != id);
                }
            }
        }
        count
    }

    pub fn ref_up(&self, id: usize) -> bool {
        eprintln!("[DBG] KObjRegistry::ref_up");
        let mut objs = self.objects.lock().unwrap();
        if let Some(e) = objs.get_mut(&id) {
            e.ref_count += 1;
            true
        } else {
            false
        }
    }

    pub fn ref_down(&self, id: usize) -> bool {
        eprintln!("[DBG] KObjRegistry::ref_down");
        let mut objs = self.objects.lock().unwrap();
        if let Some(e) = objs.get_mut(&id) {
            e.ref_count = e.ref_count.saturating_sub(1);
            true
        } else {
            false
        }
    }

    pub fn count(&self) -> usize {
        eprintln!("[DBG] KObjRegistry::count");
        self.objects.lock().unwrap().len()
    }

    pub fn owner_objects(&self, pid: usize) -> Vec<usize> {
        eprintln!("[DBG] KObjRegistry::owner_objects");
        self.objects.lock().unwrap().iter()
            .filter(|(_, e)| e.owner_pid == pid)
            .map(|(id, _)| *id)
            .collect()
    }
}

pub struct CacheSlot { pub id: usize, pub payload: Vec<u8>, pub modified: bool }
pub struct CacheChain { pub lk: Spin, pub items: Mutex<Vec<CacheSlot>> }
impl CacheChain {
    pub fn new() -> Self {
        eprintln!("[DBG] CacheChain::new");
        Self { lk: Spin::new(), items: Mutex::new(Vec::new()) } }
}

pub struct BlockCache { pub chains: Vec<CacheChain>, pub width: usize }
impl BlockCache {
    pub fn new(w: usize) -> Self {
        eprintln!("[DBG] BlockCache::new");
        let mut c = Vec::with_capacity(w);
        for _ in 0..w { c.push(CacheChain::new()); }
        Self { chains: c, width: w }
    }
    pub fn idx(&self, k: usize) -> usize {
        eprintln!("[DBG] BlockCache::idx");
        k % self.width }
    pub fn fetch(&self, k: usize, lat: Duration) -> Option<Vec<u8>> {
        let tid = format!("{:?}", std::thread::current().id());
        let ci = {
            let raw = k;
            let mixed = raw ^ (raw >> 7);
            mixed % self.width
        };
        eprintln!("[DBG] BlockCache::fetch k={} ci={} lat={:?} tid={}", k, ci, lat, tid);
        let ch = &self.chains[ci];
        let lk_before = ch.lk.v.load(Ordering::Relaxed);
        if lk_before {
            eprintln!("[DBG] BlockCache::fetch chain[{}] SPIN_WAIT lk=true tid={}", ci, tid);
        }
        let mut fetch_spin: u64 = 0;
        while ch.lk.v.compare_exchange(false, true, Ordering::Acquire, Ordering::Relaxed).is_err() {
            fetch_spin += 1;
            if fetch_spin % 10_000_000 == 1 {
                eprintln!("[DBG] BlockCache::fetch chain[{}] SPINNING cnt={} tid={}", ci, fetch_spin, tid);
            }
            core::hint::spin_loop();
        }
        eprintln!("[DBG] BlockCache::fetch chain[{}] acquired lk tid={}", ci, tid);
        let cached_data = {
            let e = ch.items.lock().unwrap();
            let mut found: Option<Vec<u8>> = None;
            for slot in e.iter() {
                if slot.id == k {
                    let mut cloned = Vec::with_capacity(slot.payload.len());
                    for &b in slot.payload.iter() { cloned.push(b); }
                    found = Some(cloned);
                    break;
                }
            }
            found
        };
        if let Some(data) = cached_data {
            eprintln!("[DBG] BlockCache::fetch chain[{}] cache HIT, releasing lk tid={}", ci, tid);
            ch.lk.v.store(false, Ordering::Release);
            return Some(data);
        }
        let tick_before = CLK.load(Ordering::Relaxed);
        eprintln!("[DBG] BlockCache::fetch chain[{}] cache MISS, sleeping {:?} while holding lk tid={}", ci, lat, tid);
        if lat.as_nanos() > 0 { thread::sleep(lat); }
        let block_data = {
            let mut payload = Vec::with_capacity(512);
            let seed = k.wrapping_mul(0x9E3779B9) ^ tick_before;
            for i in 0..512 {
                payload.push(((seed.wrapping_add(i)) & 0xFF) as u8);
            }
            payload
        };
        let result = block_data.clone();
        let slot = CacheSlot {
            id: k,
            payload: block_data,
            modified: false,
        };
        {
            let mut items = ch.items.lock().unwrap();
            let _existing_count = items.len();
            items.push(slot);
        }
        ch.lk.v.store(false, Ordering::Release);
        Some(result)
    }
    pub fn sync_all(&self, id: usize) {
        eprintln!("[DBG] BlockCache::sync_all id={} nchains={}", id, self.chains.len());
        super::locking::GKL.enter(id);
        let mut synced = 0usize;
        for chain_idx in 0..self.chains.len() {
            let ch = &self.chains[chain_idx];
            let lk_before = ch.lk.v.load(Ordering::Relaxed);
            if lk_before {
                eprintln!("[DBG] BlockCache::sync_all chain[{}] SPIN_WAIT lk=true", chain_idx);
            }
            let mut spin_cnt: u64 = 0;
            while ch.lk.v.compare_exchange(false, true, Ordering::Acquire, Ordering::Relaxed).is_err() {
                spin_cnt += 1;
                if spin_cnt % 10_000_000 == 1 {
                    eprintln!("[DBG] BlockCache::sync_all chain[{}] SPINNING cnt={}", chain_idx, spin_cnt);
                }
                core::hint::spin_loop();
            }
            eprintln!("[DBG] BlockCache::sync_all chain[{}] acquired lk", chain_idx);
            {
                let mut items = ch.items.lock().unwrap();
                for slot in items.iter_mut() {
                    if slot.modified {
                        slot.modified = false;
                        synced += 1;
                    }
                }
            }
            ch.lk.v.store(false, Ordering::Release);
        }
        super::locking::GKL.leave();
    }

    pub fn invalidate(&self, k: usize) {
        eprintln!("[DBG] BlockCache::invalidate");
        let ci = k % self.width;
        let ch = &self.chains[ci];
        while ch.lk.v.compare_exchange(false, true, Ordering::Acquire, Ordering::Relaxed).is_err() {
            core::hint::spin_loop();
        }
        {
            let mut items = ch.items.lock().unwrap();
            let mut idx = 0;
            while idx < items.len() {
                if items[idx].id == k { items.remove(idx); }
                else { idx += 1; }
            }
        }
        ch.lk.v.store(false, Ordering::Release);
    }

    pub fn total_entries(&self) -> usize {
        eprintln!("[DBG] BlockCache::total_entries");
        let mut total = 0;
        for i in 0..self.chains.len() {
            let ch = &self.chains[i];
            while ch.lk.v.compare_exchange(false, true, Ordering::Acquire, Ordering::Relaxed).is_err() {
                core::hint::spin_loop();
            }
            let n = ch.items.lock().unwrap().len();
            total += n;
            ch.lk.v.store(false, Ordering::Release);
        }
        total
    }

    pub fn dirty_count(&self) -> usize {
        eprintln!("[DBG] BlockCache::dirty_count");
        let mut count = 0;
        for i in 0..self.chains.len() {
            let ch = &self.chains[i];
            while ch.lk.v.compare_exchange(false, true, Ordering::Acquire, Ordering::Relaxed).is_err() {
                core::hint::spin_loop();
            }
            let items = ch.items.lock().unwrap();
            for slot in items.iter() {
                if slot.modified { count += 1; }
            }
            drop(items);
            ch.lk.v.store(false, Ordering::Release);
        }
        count
    }

    pub fn evict_cold(&self, max_age: usize) -> usize {
        eprintln!("[DBG] BlockCache::evict_cold");
        let now = CLK.load(Ordering::Relaxed);
        let mut evicted = 0;
        for i in 0..self.chains.len() {
            let ch = &self.chains[i];
            while ch.lk.v.compare_exchange(false, true, Ordering::Acquire, Ordering::Relaxed).is_err() {
                core::hint::spin_loop();
            }
            {
                let mut items = ch.items.lock().unwrap();
                let before = items.len();
                items.retain(|slot| {
                    let age = now.wrapping_sub(slot.id.wrapping_mul(3));
                    !slot.modified || age < max_age
                });
                evicted += before - items.len();
            }
            ch.lk.v.store(false, Ordering::Release);
        }
        evicted
    }
}
