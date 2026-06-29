use std::sync::{Arc, Mutex, Weak, RwLock};
use std::collections::BTreeMap;
use std::ops::Index;
use super::semaphore::Sema;
use super::sync_queue::{EvBus, EvFlag};
use super::consts::*;

pub type SemId = usize;
pub type SemNum = u16;
pub type SemOp = i16;

#[repr(C)]
#[derive(Clone, Copy)]
pub struct IpcPerm {
    pub key: u32,
    pub uid: u32,
    pub gid: u32,
    pub cuid: u32,
    pub cgid: u32,
    pub mode: u32,
    pub seq: u32,
    pub pad1: usize,
    pub pad2: usize,
}

#[repr(C)]
#[derive(Clone, Copy)]
pub struct SemDs {
    pub perm: IpcPerm,
    pub otime: usize,
    _p1: usize,
    pub ctime: usize,
    _p2: usize,
    pub nsems: usize,
}

pub struct SemArr {
    pub ds: Mutex<SemDs>,
    pub sems: Vec<Sema>,
}
impl Index<usize> for SemArr {
    type Output = Sema;
    fn index(&self, i: usize) -> &Sema {
        eprintln!("[DBG] Index::index");
        &self.sems[i] }
}
impl SemArr {
    pub fn remove(&self) {
        eprintln!("[DBG] SemArr::remove");
        for s in &self.sems { s.remove(); } }
    pub fn otime_now(&self) {
        eprintln!("[DBG] SemArr::otime_now");
        self.ds.lock().unwrap().otime = 0; }
    pub fn ctime_now(&self) {
        eprintln!("[DBG] SemArr::ctime_now");
        self.ds.lock().unwrap().ctime = 0; }
    pub fn set_ds(&self, new: &SemDs) {
        eprintln!("[DBG] SemArr::set_ds");
        let mut l = self.ds.lock().unwrap();
        l.perm.uid = new.perm.uid;
        l.perm.gid = new.perm.gid;
        l.perm.mode = new.perm.mode & 0x1ff;
    }
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

#[derive(Default)]
pub struct SemCtx {
    pub arrays: BTreeMap<SemId, Arc<SemArr>>,
    pub undos: BTreeMap<(SemId, SemNum), SemOp>,
}
impl SemCtx {
    pub fn add(&mut self, arr: Arc<SemArr>) -> SemId {
        eprintln!("[DBG] SemCtx::add");
        let id = (0..).find(|i| !self.arrays.contains_key(i)).unwrap();
        self.arrays.insert(id, arr);
        id
    }
    pub fn remove(&mut self, id: SemId) {
        eprintln!("[DBG] SemCtx::remove");
        self.arrays.remove(&id); }
    fn free_id(&self) -> SemId {
        eprintln!("[DBG] SemCtx::free_id");
        (0..).find(|i| self.arrays.get(i).is_none()).unwrap() }
    pub fn get(&self, id: SemId) -> Option<Arc<SemArr>> {
        eprintln!("[DBG] SemCtx::get");
        self.arrays.get(&id).cloned() }
    pub fn add_undo(&mut self, id: SemId, num: SemNum, op: SemOp) {
        eprintln!("[DBG] SemCtx::add_undo");
        let old = *self.undos.get(&(id, num)).unwrap_or(&0);
        self.undos.insert((id, num), old - op);
    }
}
impl Clone for SemCtx {
    fn clone(&self) -> Self {
        eprintln!("[DBG] Clone::clone");
        SemCtx { arrays: self.arrays.clone(), undos: BTreeMap::new() }
    }
}
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

type ShmId = usize;

#[derive(Clone)]
pub struct ShmTag {
    pub addr: usize,
    pub pages: Arc<Mutex<Vec<usize>>>,
}
impl ShmTag {
    pub fn set_addr(&mut self, a: usize) {
        eprintln!("[DBG] ShmTag::set_addr");
        self.addr = a; }
}

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

#[derive(Default)]
pub struct ShmCtx { pub ids: BTreeMap<ShmId, ShmTag> }
impl ShmCtx {
    pub fn add(&mut self, g: Arc<Mutex<Vec<usize>>>) -> ShmId {
        eprintln!("[DBG] ShmCtx::add");
        let id = (0..).find(|i| !self.ids.contains_key(i)).unwrap();
        self.ids.insert(id, ShmTag { addr: 0, pages: g });
        id
    }
    pub fn get(&self, id: ShmId) -> Option<ShmTag> {
        eprintln!("[DBG] ShmCtx::get");
        self.ids.get(&id).cloned() }
    pub fn set(&mut self, id: ShmId, tag: ShmTag) {
        eprintln!("[DBG] ShmCtx::set");
        self.ids.insert(id, tag); }
    pub fn get_id_by_addr(&self, addr: usize) -> Option<ShmId> {
        eprintln!("[DBG] ShmCtx::get_id_by_addr");
        self.ids.iter().find(|(_, v)| v.addr == addr).map(|(k, _)| *k)
    }
    pub fn pop(&mut self, id: ShmId) {
        eprintln!("[DBG] ShmCtx::pop");
        self.ids.remove(&id); }
}
impl Clone for ShmCtx {
    fn clone(&self) -> Self {
        eprintln!("[DBG] Clone::clone");
        ShmCtx { ids: self.ids.clone() } }
}
