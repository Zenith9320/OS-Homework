//! 内核缓存模块：提供页面缓存、内核对象注册表以及分块并发块缓存。
//!
//! 本模块实现了三类缓存抽象：
//! - [`PageCache`]：基于 LRU 的页面缓存，支持 pin/unpin、脏页追踪和逐出。
//! - [`KObjRegistry`]：内核对象注册表，按类型和所有者索引，支持引用计数与 GC 回收。
//! - [`BlockCache`]：分多链并发访问的块缓存，使用自旋锁保护每条链。

use std::sync::{Arc, Mutex};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::collections::{HashMap, VecDeque, BTreeMap};
use std::time::Duration;
use std::thread;
use std::cmp::min;
use super::locking::Spin;
use super::CLK;

/// 页面缓存中的单个条目，代表一个被缓存的内存页。
pub struct PageCacheEntry {
    /// 页面唯一标识符
    pub page_id: usize,
    /// 页面的数据内容
    pub data: Vec<u8>,
    /// 是否为脏页（已修改但尚未写回）
    pub dirty: bool,
    /// 最后一次被访问时的逻辑时钟滴答值
    pub access_tick: usize,
    /// 当前被固定（pin）的次数，大于 0 时不可被逐出
    pub pin_count: usize,
}

/// 基于 LRU 策略的页面缓存。
///
/// 维护一个固定容量的页面条目集合，自动统计命中、未命中和逐出次数，
/// 并通过 LRU 链表决定逐出顺序。
pub struct PageCache {
    /// 缓存的页面条目，按 page_id 索引
    pub entries: HashMap<usize, PageCacheEntry>,
    /// 缓存最大容量（条目数）
    pub capacity: usize,
    /// 累计缓存命中次数（原子变量，支持无锁统计）
    pub hits: AtomicUsize,
    /// 累计缓存未命中次数（原子变量）
    pub misses: AtomicUsize,
    /// 累计逐出次数（原子变量）
    pub evictions: AtomicUsize,
    /// LRU 顺序队列，尾部为最近访问的页面
    pub lru_order: VecDeque<usize>,
}

impl PageCache {
    /// 创建一个指定容量的空页面缓存。
    ///
    /// # 参数
    ///
    /// * `capacity` - 最大可缓存的页面条目数
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

    /// 查找指定页面。
    ///
    /// 命中时：更新统计计数、调整 LRU 顺序并刷新访问时间戳，返回页面数据切片。
    /// 未命中时：增加未命中计数并返回 `None`。
    ///
    /// # 参数
    ///
    /// * `page_id` - 要查找的页面 ID
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

    /// 插入一个新页面条目。
    ///
    /// 如果缓存已满，会先调用 [`evict_lru`](Self::evict_lru) 逐出一个未 pin 的条目。
    ///
    /// # 参数
    ///
    /// * `page_id` - 页面的唯一标识符
    /// * `data` - 页面的数据内容
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

    /// 按 LRU 顺序逐出一个 pin_count 为 0 的页面。
    ///
    /// 返回 `true` 表示成功逐出，`false` 表示没有可逐出的条目（所有条目均被 pin）。
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

    /// 将指定页面标记为脏页。
    ///
    /// # 参数
    ///
    /// * `page_id` - 要标记为脏页的页面 ID
    pub fn mark_dirty(&mut self, page_id: usize) {
        eprintln!("[DBG] PageCache::mark_dirty");
        if let Some(e) = self.entries.get_mut(&page_id) {
            e.dirty = true;
        }
    }

    /// 将所有脏页写回（清除 dirty 标记）。
    ///
    /// 返回实际写回的脏页数量。
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

    /// 返回缓存统计信息：(命中数, 未命中数, 逐出数)。
    pub fn stats(&self) -> (usize, usize, usize) {
        eprintln!("[DBG] PageCache::stats");
        (
            self.hits.load(Ordering::Relaxed),
            self.misses.load(Ordering::Relaxed),
            self.evictions.load(Ordering::Relaxed),
        )
    }

    /// 固定（pin）指定页面，使其不可被逐出。
    ///
    /// 返回 `true` 表示固定成功，`false` 表示页面不存在。
    ///
    /// # 参数
    ///
    /// * `page_id` - 要固定的页面 ID
    pub fn pin(&mut self, page_id: usize) -> bool {
        eprintln!("[DBG] PageCache::pin");
        if let Some(e) = self.entries.get_mut(&page_id) {
            e.pin_count += 1;
            true
        } else {
            false
        }
    }

    /// 解除固定（unpin）指定页面。
    ///
    /// 返回 `true` 表示操作成功（页面存在），`false` 表示页面不存在。
    /// pin_count 减到 0 后该页面可被逐出。
    ///
    /// # 参数
    ///
    /// * `page_id` - 要解除固定的页面 ID
    pub fn unpin(&mut self, page_id: usize) -> bool {
        eprintln!("[DBG] PageCache::unpin");
        if let Some(e) = self.entries.get_mut(&page_id) {
            if e.pin_count > 0 { e.pin_count -= 1; }
            true
        } else {
            false
        }
    }

    /// 使指定页面失效并从缓存中移除。
    ///
    /// 返回 `true` 表示成功移除，`false` 表示页面不存在。
    ///
    /// # 参数
    ///
    /// * `page_id` - 要失效的页面 ID
    pub fn invalidate(&mut self, page_id: usize) -> bool {
        eprintln!("[DBG] PageCache::invalidate");
        if self.entries.remove(&page_id).is_some() {
            self.lru_order.retain(|&x| x != page_id);
            true
        } else {
            false
        }
    }

    /// 将指定 ID 范围内的脏页全部写回（清除 dirty 标记）。
    ///
    /// 返回实际写回的页面数量。
    ///
    /// # 参数
    ///
    /// * `start` - 起始页面 ID（包含）
    /// * `end` - 结束页面 ID（不包含）
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

/// 内核对象注册表中的单个条目，代表一个被追踪的内核对象。
pub struct KObjEntry {
    /// 内核对象唯一 ID
    pub obj_id: usize,
    /// 对象类型标签，用于按类型分类检索
    pub type_tag: u32,
    /// 拥有该对象的进程 ID
    pub owner_pid: usize,
    /// 对象创建时刻的逻辑时钟滴答值
    pub created_tick: usize,
    /// 引用计数，降为 0 时可被 GC 回收
    pub ref_count: usize,
    /// 父对象 ID，若为 `None` 则表示该对象为顶层对象
    pub parent_id: Option<usize>,
}

/// 内核对象注册表：管理所有内核对象的生命周期、类型索引和父子关系。
///
/// 使用互斥锁保护内部数据结构，通过原子变量分配自增 ID。
pub struct KObjRegistry {
    /// 对象存储，按 ID 排序的 BTreeMap，受 Mutex 保护
    pub objects: Mutex<BTreeMap<usize, KObjEntry>>,
    /// 自增 ID 序列号，用于分配新对象 ID（原子变量）
    pub seq: AtomicUsize,
    /// 按类型标签索引的对象 ID 列表，受 Mutex 保护
    pub type_index: Mutex<BTreeMap<u32, Vec<usize>>>,
}

impl KObjRegistry {
    /// 创建一个空的内核对象注册表。
    ///
    /// ID 序列从 1 开始（0 保留为无效）。
    pub fn new() -> Self {
        eprintln!("[DBG] KObjRegistry::new");
        Self {
            objects: Mutex::new(BTreeMap::new()),
            seq: AtomicUsize::new(1),
            type_index: Mutex::new(BTreeMap::new()),
        }
    }

    /// 注册一个新的内核对象。
    ///
    /// 返回新分配的对象 ID。初始引用计数为 1。
    ///
    /// # 参数
    ///
    /// * `type_tag` - 对象类型标签
    /// * `owner_pid` - 所有者进程 ID
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

    /// 注册一个带父对象的内核子对象。
    ///
    /// 返回新分配的对象 ID。初始引用计数为 1。
    ///
    /// # 参数
    ///
    /// * `type_tag` - 对象类型标签
    /// * `owner_pid` - 所有者进程 ID
    /// * `parent` - 父对象 ID
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

    /// 注销指定 ID 的内核对象，同时从类型索引中移除。
    ///
    /// 返回 `true` 表示注销成功，`false` 表示对象不存在。
    ///
    /// # 参数
    ///
    /// * `id` - 要注销的对象 ID
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

    /// 按类型标签查找所有匹配的对象 ID。
    ///
    /// 返回匹配的对象 ID 列表（若无匹配则返回空 Vec）。
    ///
    /// # 参数
    ///
    /// * `tag` - 要查找的类型标签
    pub fn find_by_type(&self, tag: u32) -> Vec<usize> {
        eprintln!("[DBG] KObjRegistry::find_by_type");
        self.type_index.lock().unwrap().get(&tag).cloned().unwrap_or_default()
    }

    /// 导出对象之间的父子关系图。
    ///
    /// 返回一个 `(parent_id, child_id)` 元组的向量，每条边代表一对父子关系。
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

    /// 垃圾回收：扫描并移除所有引用计数为 0 的对象。
    ///
    /// 返回被回收的对象数量。
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

    /// 增加指定对象的引用计数。
    ///
    /// 返回 `true` 表示成功，`false` 表示对象不存在。
    ///
    /// # 参数
    ///
    /// * `id` - 目标对象 ID
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

    /// 减少指定对象的引用计数（使用饱和减法，不会下溢到负数）。
    ///
    /// 返回 `true` 表示成功，`false` 表示对象不存在。
    ///
    /// # 参数
    ///
    /// * `id` - 目标对象 ID
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

    /// 返回当前注册表中的对象总数。
    pub fn count(&self) -> usize {
        eprintln!("[DBG] KObjRegistry::count");
        self.objects.lock().unwrap().len()
    }

    /// 查找指定进程拥有的所有对象 ID。
    ///
    /// # 参数
    ///
    /// * `pid` - 进程 ID
    pub fn owner_objects(&self, pid: usize) -> Vec<usize> {
        eprintln!("[DBG] KObjRegistry::owner_objects");
        self.objects.lock().unwrap().iter()
            .filter(|(_, e)| e.owner_pid == pid)
            .map(|(id, _)| *id)
            .collect()
    }
}

/// 块缓存中的一个槽位，存储一个块的数据及其状态。
pub struct CacheSlot {
    /// 槽位对应的键（块 ID）
    pub id: usize,
    /// 块的有效载荷数据
    pub payload: Vec<u8>,
    /// 该块是否已被修改
    pub modified: bool,
}

/// 块缓存中的一条链，由自旋锁保护的一个 CacheSlot 列表。
///
/// 每条链独立加锁，从而在多线程并发访问时减少锁竞争。
pub struct CacheChain {
    /// 保护本条链的自旋锁
    pub lk: Spin,
    /// 链中存储的缓存槽位列表，受 Mutex 保护
    pub items: Mutex<Vec<CacheSlot>>,
}

impl CacheChain {
    /// 创建一条新的空缓存链。
    pub fn new() -> Self {
        eprintln!("[DBG] CacheChain::new");
        Self { lk: Spin::new(), items: Mutex::new(Vec::new()) }
    }
}

/// 分链并发块缓存。
///
/// 将缓存空间划分为多条独立的链（宽度为 `width`），
/// 每条链由各自的自旋锁保护，键通过哈希分散到各链以减少锁竞争。
pub struct BlockCache {
    /// 缓存链数组，每条链独立加锁
    pub chains: Vec<CacheChain>,
    /// 链的数量（宽度），也是取模分片的模数
    pub width: usize,
}

impl BlockCache {
    /// 创建一个指定宽度的块缓存。
    ///
    /// # 参数
    ///
    /// * `w` - 链的数量，必须大于 0
    pub fn new(w: usize) -> Self {
        eprintln!("[DBG] BlockCache::new");
        let mut c = Vec::with_capacity(w);
        for _ in 0..w { c.push(CacheChain::new()); }
        Self { chains: c, width: w }
    }

    /// 计算给定键对应的链索引（简单取模）。
    ///
    /// # 参数
    ///
    /// * `k` - 键值
    pub fn idx(&self, k: usize) -> usize {
        eprintln!("[DBG] BlockCache::idx");
        k % self.width
    }

    /// 从块缓存中获取指定键的数据。
    ///
    /// 命中时直接返回缓存数据。未命中时：模拟一个延迟（`lat`）后
    /// 生成伪随机的块数据，将其插入缓存并返回。
    ///
    /// 操作期间会持有对应链的自旋锁，以确保线程安全。
    ///
    /// # 参数
    ///
    /// * `k` - 要获取的块键值
    /// * `lat` - 模拟的读取延迟
    pub fn fetch(&self, k: usize, lat: Duration, disk: &super::io::Disk) -> Option<Vec<u8>> {
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
        eprintln!("[DBG] BlockCache::fetch chain[{}] cache MISS, reading from disk tid={}", ci, tid);
        // 未命中：从磁盘读取
        let mut block_data = vec![0u8; 512];
        match disk.read_block(k, &mut block_data) {
            Ok(()) => {}
            Err(_) => { ch.lk.v.store(false, Ordering::Release); return None; }
        }
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

    /// 将数据写入缓存并标记为脏。如果该块已缓存则更新，否则新建条目。
    pub fn put(&self, k: usize, data: Vec<u8>) {
        let ci = {
            let raw = k;
            let mixed = raw ^ (raw >> 7);
            mixed % self.width
        };
        let ch = &self.chains[ci];
        // 获取链自旋锁
        while ch.lk.v.compare_exchange(false, true, Ordering::Acquire, Ordering::Relaxed).is_err() {
            core::hint::spin_loop();
        }
        let mut items = ch.items.lock().unwrap();
        // 查找已存在的条目
        for slot in items.iter_mut() {
            if slot.id == k {
                slot.payload = data;
                slot.modified = true;
                ch.lk.v.store(false, Ordering::Release);
                return;
            }
        }
        // 不存在，新建
        items.push(CacheSlot { id: k, payload: data, modified: true });
        ch.lk.v.store(false, Ordering::Release);
    }

    /// 同步所有链中被修改的块（清除 modified 标记）。
    ///
    /// 在进入和退出时分别调用全局内核锁的 enter/leave，确保全局同步语义。
    /// 依次遍历所有链，对每条链获取自旋锁后清除所有脏 slot 的标记。
    ///
    /// # 参数
    ///
    /// * `id` - 同步操作的标识符，传递给全局锁 enter
    pub fn sync_all(&self, id: usize, disk: &super::io::Disk) {
        eprintln!("[DBG] BlockCache::sync_all id={} nchains={}", id, self.chains.len());
        super::locking::GKL.enter(id);
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
                        let _ = disk.write_block(slot.id, &slot.payload);
                        slot.modified = false;
                    }
                }
            }
            ch.lk.v.store(false, Ordering::Release);
        }
        super::locking::GKL.leave(id);
    }

    /// 使指定键对应的所有缓存槽位失效并从对应链中删除。
    ///
    /// # 参数
    ///
    /// * `k` - 要失效的键值
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

    /// 返回所有链中的缓存条目总数。
    ///
    /// 遍历每条链获取锁后统计，确保计数准确。
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

    /// 返回所有链中被标记为 modified 的槽位总数。
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

    /// 逐出所有“冷”缓存条目。
    ///
    /// 对于每个槽位，根据当前时钟和槽位 ID 估算其年龄。
    /// 已修改但年龄未超过 `max_age` 的槽位会被保留。
    /// 返回被逐出的槽位总数。
    ///
    /// # 参数
    ///
    /// * `max_age` - 可容忍的最大年龄阈值
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
