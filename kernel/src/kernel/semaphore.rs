use std::sync::{Arc, Mutex};
use std::thread;
use std::ops::{Deref, DerefMut};
use super::sync_queue::{EvBus, EvFlag};

struct SemaInner { pub cnt: isize, pub pid: usize, pub rm: bool, pub bus: EvBus }

pub struct Sema { inner: Arc<Mutex<SemaInner>> }

pub struct SemaGuard<'a> { s: &'a Sema }

impl Sema {
    pub fn new(c: isize) -> Self {
        eprintln!("[DBG] Sema::new");
        Sema { inner: Arc::new(Mutex::new(SemaInner { cnt: c, rm: false, pid: 0, bus: EvBus::default() })) }
    }
    pub fn remove(&self) {
        eprintln!("[DBG] Sema::remove");
        let mut i = self.inner.lock().unwrap();
        i.rm = true;
        i.bus.set(EvFlag::SEM_RM);
    }
    pub fn release(&self) {
        eprintln!("[DBG] Sema::release");
        let mut i = self.inner.lock().unwrap();
        i.cnt += 1;
        if i.cnt >= 1 { i.bus.set(EvFlag::SEM_ACQ); }
    }
    pub fn try_acquire(&self) -> Result<bool, &'static str> {
        eprintln!("[DBG] Sema::try_acquire");
        let mut i = self.inner.lock().unwrap();
        if i.rm { return Err("removed"); }
        if i.cnt >= 1 {
            i.cnt -= 1;
            if i.cnt < 1 { i.bus.clear(EvFlag::SEM_ACQ); }
            Ok(true)
        } else {
            Ok(false)
        }
    }
    pub fn acquire_spin(&self) -> Result<(), &'static str> {
        eprintln!("[DBG] Sema::acquire_spin");
        loop {
            match self.try_acquire()? {
                true => return Ok(()),
                false => thread::yield_now(),
            }
        }
    }
    pub fn access(&self) -> Result<SemaGuard<'_>, &'static str> {
        eprintln!("[DBG] Sema::access");
        self.acquire_spin()?;
        Ok(SemaGuard { s: self })
    }
    pub fn get_val(&self) -> isize {
        eprintln!("[DBG] Sema::get_val");
        self.inner.lock().unwrap().cnt }
    pub fn get_ncnt(&self) -> usize {
        eprintln!("[DBG] Sema::get_ncnt");
        self.inner.lock().unwrap().bus.cb_len() }
    pub fn get_pid(&self) -> usize {
        eprintln!("[DBG] Sema::get_pid");
        self.inner.lock().unwrap().pid }
    pub fn set_pid(&self, p: usize) {
        eprintln!("[DBG] Sema::set_pid");
        self.inner.lock().unwrap().pid = p; }
    pub fn set_val(&self, v: isize) {
        eprintln!("[DBG] Sema::set_val");
        let mut i = self.inner.lock().unwrap();
        i.cnt = v;
        if i.cnt >= 1 { i.bus.set(EvFlag::SEM_ACQ); }
    }
}

impl<'a> Drop for SemaGuard<'a> { fn drop(&mut self) { self.s.release(); } }
impl<'a> Deref for SemaGuard<'a> {
    type Target = Sema;
    fn deref(&self) -> &Self::Target {
        eprintln!("[DBG] Deref::deref");
        self.s }
}
