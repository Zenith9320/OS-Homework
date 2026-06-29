//! 进程间通信（IPC）模块。
//!
//! 本模块实现了 System V 风格的信号量（Semaphore）和共享内存（Shared Memory）
//! 机制，用于支持进程之间的同步与数据交换。主要包括：
//! - 信号量集合（`SemArr`）及其上下文（`SemCtx`）
//! - 共享内存标签（`ShmTag`）及其上下文（`ShmCtx`）
//! - IPC 权限结构体（`IpcPerm`）和信号量描述结构体（`SemDs`）

use std::sync::{Arc, Mutex, Weak, RwLock};
use std::collections::BTreeMap;
use std::ops::Index;
use super::semaphore::Sema;
use super::sync_queue::{EvBus, EvFlag};
use super::consts::*;

/// 信号量集合的唯一标识符类型。
pub type SemId = usize;
/// 信号量在集合中的编号类型。
pub type SemNum = u16;
/// 信号量操作值类型（可为负表示 P 操作，正表示 V 操作）。
pub type SemOp = i16;

/// IPC 权限信息结构体，与 C 语言 `ipc_perm` 结构体布局兼容（`#[repr(C)]`）。
///
/// 记录 IPC 对象的所有权及访问权限信息。
#[repr(C)]
#[derive(Clone, Copy)]
pub struct IpcPerm {
    /// IPC 对象的键值（key），用于跨进程查找同一 IPC 资源。
    pub key: u32,
    /// 所有者的用户 ID。
    pub uid: u32,
    /// 所有者的组 ID。
    pub gid: u32,
    /// 创建者的用户 ID。
    pub cuid: u32,
    /// 创建者的组 ID。
    pub cgid: u32,
    /// 访问权限模式位（类似文件的 rwx 权限）。
    pub mode: u32,
    /// 序列号，用于区分重复使用同一 ID 的 IPC 对象。
    pub seq: u32,
    /// 填充字段，用于内存对齐（与 C 结构体兼容）。
    pub pad1: usize,
    /// 填充字段，用于内存对齐（与 C 结构体兼容）。
    pub pad2: usize,
}

/// 信号量描述结构体，与 C 语言 `semid_ds` 结构体布局兼容（`#[repr(C)]`）。
///
/// 封装信号量集合的元数据和权限信息。
#[repr(C)]
#[derive(Clone, Copy)]
pub struct SemDs {
    /// IPC 权限信息。
    pub perm: IpcPerm,
    /// 最后一次信号量操作（semop）的时间戳。
    pub otime: usize,
    /// 内部填充字段（与 C 结构体对齐）。
    _p1: usize,
    /// 最后一次信号量控制（semctl）修改的时间戳。
    pub ctime: usize,
    /// 内部填充字段（与 C 结构体对齐）。
    _p2: usize,
    /// 该信号量集合中包含的信号量数量。
    pub nsems: usize,
}

/// 信号量数组集合。
///
/// 表示一个信号量集合，包含一组信号量以及描述该集合的元数据（`SemDs`）。
pub struct SemArr {
    /// 信号量集合的描述信息（受互斥锁保护）。
    pub ds: Mutex<SemDs>,
    /// 集合中的各个信号量。
    pub sems: Vec<Sema>,
}

/// 为 `SemArr` 实现 `Index` trait，使可以通过索引直接访问集合中的信号量。
impl Index<usize> for SemArr {
    type Output = Sema;
    /// 根据索引获取对应信号量的引用。
    fn index(&self, i: usize) -> &Sema {
        eprintln!("[DBG] Index::index");
        &self.sems[i] }
}

impl SemArr {
    /// 移除该信号量集合中的所有信号量。
    pub fn remove(&self) {
        eprintln!("[DBG] SemArr::remove");
        for s in &self.sems { s.remove(); } }

    /// 将操作时间戳 `otime` 设置为当前时间（此处简化为 0）。
    pub fn otime_now(&self) {
        eprintln!("[DBG] SemArr::otime_now");
        self.ds.lock().unwrap().otime = 0; }

    /// 将修改时间戳 `ctime` 设置为当前时间（此处简化为 0）。
    pub fn ctime_now(&self) {
        eprintln!("[DBG] SemArr::ctime_now");
        self.ds.lock().unwrap().ctime = 0; }

    /// 用传入的描述信息更新该信号量集合的元数据。
    ///
    /// 仅更新所有者 UID、组 GID 和权限模式位（低 9 位）。
    pub fn set_ds(&self, new: &SemDs) {
        eprintln!("[DBG] SemArr::set_ds");
        let mut l = self.ds.lock().unwrap();
        l.perm.uid = new.perm.uid;
        l.perm.gid = new.perm.gid;
        l.perm.mode = new.perm.mode & 0x1ff;
    }

    /// 根据键值获取或创建一个信号量集合。
    ///
    /// - 若 `key` 为 0（`IPC_PRIVATE`），则会自动分配一个新的键值。
    /// - 若 `key` 不为 0 且已存在对应集合，则返回已有集合。
    /// - 若同时指定了 `IPC_CREAT` 和 `IPC_EXCL` 标志且集合已存在，则返回错误。
    /// - 否则创建一个新的信号量集合并存入全局存储。
    pub fn get_or_create(
        key: u32,
        nsems: usize,
        flags: usize,
        store: &RwLock<BTreeMap<u32, Weak<SemArr>>>,
    ) -> Result<Arc<Self>, &'static str> {
        eprintln!("[DBG] SemArr::get_or_create");
        let mut m = store.write().unwrap();
        let mut k = key;
        if k == 0 {
            k = (1u32..).find(|i| m.get(i).is_none()).unwrap();
        } else if let Some(w) = m.get(&k) {
            if let Some(a) = w.upgrade() {
                if (flags & (1 << 9)) != 0 && (flags & (1 << 10)) != 0 { return Err("eexist"); }
                return Ok(a);
            }
        }
        let mut sv = Vec::new();
        for _ in 0..nsems { sv.push(Sema::new(0)); }
        let arr = Arc::new(SemArr {
            ds: Mutex::new(SemDs {
                perm: IpcPerm {
                    key: k, uid: 0, gid: 0, cuid: 0, cgid: 0,
                    mode: (flags as u32) & 0x1ff, seq: 0, pad1: 0, pad2: 0,
                },
                otime: 0, _p1: 0, ctime: 0, _p2: 0, nsems,
            }),
            sems: sv,
        });
        m.insert(k, Arc::downgrade(&arr));
        Ok(arr)
    }
}

/// 信号量上下文。
///
/// 每个进程持有自己的 `SemCtx`，维护该进程已打开的信号量集合
/// 以及待撤销的信号量操作记录（用于进程异常退出时的自动回滚）。
#[derive(Default)]
pub struct SemCtx {
    /// 已打开的信号量集合，按 ID 索引。
    pub arrays: BTreeMap<SemId, Arc<SemArr>>,
    /// 待撤销的操作记录，键为 `(信号量集合ID, 信号量编号)`，值为累计的操作值。
    pub undos: BTreeMap<(SemId, SemNum), SemOp>,
}

impl SemCtx {
    /// 向上下文中添加一个信号量集合，返回分配的唯一 ID。
    pub fn add(&mut self, arr: Arc<SemArr>) -> SemId {
        eprintln!("[DBG] SemCtx::add");
        let id = (0..).find(|i| !self.arrays.contains_key(i)).unwrap();
        self.arrays.insert(id, arr);
        id
    }

    /// 根据 ID 从上下文中移除一个信号量集合。
    pub fn remove(&mut self, id: SemId) {
        eprintln!("[DBG] SemCtx::remove");
        self.arrays.remove(&id); }

    /// 查找一个可用的（未被占用的）信号量集合 ID。
    fn free_id(&self) -> SemId {
        eprintln!("[DBG] SemCtx::free_id");
        (0..).find(|i| self.arrays.get(i).is_none()).unwrap() }

    /// 根据 ID 获取信号量集合的引用。
    pub fn get(&self, id: SemId) -> Option<Arc<SemArr>> {
        eprintln!("[DBG] SemCtx::get");
        self.arrays.get(&id).cloned() }

    /// 记录一条撤销操作。
    ///
    /// 当进程异常退出时，需要根据撤销记录
    /// 逆转之前执行的信号量操作，防止死锁或资源泄露。
    pub fn add_undo(&mut self, id: SemId, num: SemNum, op: SemOp) {
        eprintln!("[DBG] SemCtx::add_undo");
        let old = *self.undos.get(&(id, num)).unwrap_or(&0);
        self.undos.insert((id, num), old - op);
    }
}

/// 克隆信号量上下文时，仅克隆已打开的信号量集合引用，
/// 不克隆撤销操作记录（新进程不应继承旧的撤销记录）。
impl Clone for SemCtx {
    fn clone(&self) -> Self {
        eprintln!("[DBG] Clone::clone");
        SemCtx { arrays: self.arrays.clone(), undos: BTreeMap::new() }
    }
}

/// 信号量上下文被丢弃时，自动执行所有记录的撤销操作。
///
/// 遍历所有待撤销记录，对操作值为 1 的信号量执行释放（release）操作，
/// 确保内核资源得到正确的恢复。
impl Drop for SemCtx {
    fn drop(&mut self) {
        eprintln!("[DBG] Drop::drop");
        for (&(id, num), &op) in &self.undos {
            if let Some(arr) = self.arrays.get(&id) {
                match op {
                    1 => arr[num as usize].release(),
                    _ => {}
                }
            }
        }
    }
}

/// 共享内存区域标识符类型。
type ShmId = usize;

/// 共享内存区域标签。
///
/// 记录一块共享内存在当前进程中的映射地址和物理页框集合。
#[derive(Clone)]
pub struct ShmTag {
    /// 共享内存在当前进程地址空间中的虚拟地址。
    pub addr: usize,
    /// 共享内存占用的物理页框编号列表（通过 `Arc<Mutex<Vec<usize>>>` 共享）。
    pub pages: Arc<Mutex<Vec<usize>>>,
}

impl ShmTag {
    /// 设置共享内存在当前进程中的映射地址。
    pub fn set_addr(&mut self, a: usize) {
        eprintln!("[DBG] ShmTag::set_addr");
        self.addr = a; }
}

/// 根据键值获取或创建一个共享内存区域。
///
/// 若 `key` 对应的共享内存已存在（且其 `Weak` 引用仍可升级），
/// 则返回已有区域；否则分配新的物理页框集合并注册到全局存储。
pub fn shm_get_or_create(
    key: usize,
    npages: usize,
    store: &RwLock<BTreeMap<usize, Weak<Mutex<Vec<usize>>>>>,
) -> Arc<Mutex<Vec<usize>>> {
    eprintln!("[DBG] shm_get_or_create");
    let mut m = store.write().unwrap();
    if let Some(w) = m.get(&key) {
        if let Some(g) = w.upgrade() { return g; }
    }
    let g = Arc::new(Mutex::new(vec![0usize; npages]));
    m.insert(key, Arc::downgrade(&g));
    g
}

/// 共享内存上下文。
///
/// 每个进程持有自己的 `ShmCtx`，维护该进程已附加的共享内存区域。
#[derive(Default)]
pub struct ShmCtx {
    /// 已附加的共享内存区域，按 ID 索引。
    pub ids: BTreeMap<ShmId, ShmTag>,
}

impl ShmCtx {
    /// 向上下文中添加一个共享内存区域，返回分配的唯一 ID。
    pub fn add(&mut self, g: Arc<Mutex<Vec<usize>>>) -> ShmId {
        eprintln!("[DBG] ShmCtx::add");
        let id = (0..).find(|i| !self.ids.contains_key(i)).unwrap();
        self.ids.insert(id, ShmTag { addr: 0, pages: g });
        id
    }

    /// 根据 ID 获取共享内存标签的克隆。
    pub fn get(&self, id: ShmId) -> Option<ShmTag> {
        eprintln!("[DBG] ShmCtx::get");
        self.ids.get(&id).cloned() }

    /// 设置指定 ID 的共享内存标签（更新其映射信息）。
    pub fn set(&mut self, id: ShmId, tag: ShmTag) {
        eprintln!("[DBG] ShmCtx::set");
        self.ids.insert(id, tag); }

    /// 根据虚拟地址查找对应的共享内存 ID。
    pub fn get_id_by_addr(&self, addr: usize) -> Option<ShmId> {
        eprintln!("[DBG] ShmCtx::get_id_by_addr");
        self.ids.iter().find(|(_, v)| v.addr == addr).map(|(k, _)| *k)
    }

    /// 根据 ID 移除一个共享内存区域。
    pub fn pop(&mut self, id: ShmId) {
        eprintln!("[DBG] ShmCtx::pop");
        self.ids.remove(&id); }
}

/// 克隆共享内存上下文时，深拷贝其内部的 ID 映射表。
impl Clone for ShmCtx {
    fn clone(&self) -> Self {
        eprintln!("[DBG] Clone::clone");
        ShmCtx { ids: self.ids.clone() } }
}
