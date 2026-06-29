//! 信号量模块。
//!
//! 实现计数信号量（Counting Semaphore），类似于 POSIX `sem_t`。
//! 提供 RAII 守卫 `SemaGuard` 用于自动释放。

use std::sync::{Arc, Mutex};
use std::thread;
use std::ops::{Deref, DerefMut};
use super::sync_queue::{EvBus, EvFlag};

/// 信号量内部状态。
///
/// 成员：
/// - `cnt`: 当前可用资源计数（<= 0 表示需要等待）
/// - `pid`: 上次操作此信号量的进程 ID
/// - `rm`: 是否已被标记为删除
/// - `bus`: 关联的事件总线（用于通知等待者）
struct SemaInner { pub cnt: isize, pub pid: usize, pub rm: bool, pub bus: EvBus }

/// 计数信号量。
///
/// 包装在 `Arc<Mutex<SemaInner>>` 中，支持多线程共享。
/// 初始化时指定初始计数 `c`。
pub struct Sema { inner: Arc<Mutex<SemaInner>> }

/// 信号量 RAII 守卫。
///
/// 通过 `Sema::access()` 获取。析构时自动调用 `release()` 归还资源。
pub struct SemaGuard<'a> { s: &'a Sema }

impl Sema {
    /// 创建一个初始计数为 `c` 的信号量。
    pub fn new(c: isize) -> Self {
        eprintln!("[DBG] Sema::new");
        Sema { inner: Arc::new(Mutex::new(SemaInner { cnt: c, rm: false, pid: 0, bus: EvBus::default() })) }
    }
    /// 标记信号量为已删除状态，并发送 `SEM_RM` 事件通知所有等待者。
    pub fn remove(&self) {
        eprintln!("[DBG] Sema::remove");
        let mut i = self.inner.lock().unwrap();
        i.rm = true;
        i.bus.set(EvFlag::SEM_RM);
    }
    /// 释放一个资源（计数 +1）。
    ///
    /// 如果释放后计数 >= 1，发送 `SEM_ACQ` 事件通知等待者。
    pub fn release(&self) {
        eprintln!("[DBG] Sema::release");
        let mut i = self.inner.lock().unwrap();
        i.cnt += 1;
        if i.cnt >= 1 { i.bus.set(EvFlag::SEM_ACQ); }
    }
    /// 尝试获取一个资源（非阻塞）。
    ///
    /// 成功返回 `Ok(true)`，资源不足返回 `Ok(false)`，
    /// 信号量已被删除返回 `Err("removed")`。
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
    /// 自旋等待直到成功获取资源。
    ///
    /// 循环调用 `try_acquire()`，失败时让出 CPU。
    pub fn acquire_spin(&self) -> Result<(), &'static str> {
        eprintln!("[DBG] Sema::acquire_spin");
        loop {
            match self.try_acquire()? {
                true => return Ok(()),
                false => thread::yield_now(),
            }
        }
    }
    /// 获取资源并返回 RAII 守卫。
    ///
    /// 内部调用 `acquire_spin()`，守卫析构时自动 `release()`。
    pub fn access(&self) -> Result<SemaGuard<'_>, &'static str> {
        eprintln!("[DBG] Sema::access");
        self.acquire_spin()?;
        Ok(SemaGuard { s: self })
    }
    /// 获取当前资源计数。
    pub fn get_val(&self) -> isize {
        eprintln!("[DBG] Sema::get_val");
        self.inner.lock().unwrap().cnt }
    /// 获取等待者数量（通过事件总线回调数估算）。
    pub fn get_ncnt(&self) -> usize {
        eprintln!("[DBG] Sema::get_ncnt");
        self.inner.lock().unwrap().bus.cb_len() }
    /// 获取上次操作此信号量的进程 ID。
    pub fn get_pid(&self) -> usize {
        eprintln!("[DBG] Sema::get_pid");
        self.inner.lock().unwrap().pid }
    /// 设置与此信号量关联的进程 ID。
    pub fn set_pid(&self, p: usize) {
        eprintln!("[DBG] Sema::set_pid");
        self.inner.lock().unwrap().pid = p; }
    /// 直接设置资源计数（用于 semctl SETVAL）。
    pub fn set_val(&self, v: isize) {
        eprintln!("[DBG] Sema::set_val");
        let mut i = self.inner.lock().unwrap();
        i.cnt = v;
        if i.cnt >= 1 { i.bus.set(EvFlag::SEM_ACQ); }
    }
}

impl<'a> Drop for SemaGuard<'a> {
    /// 守卫析构时自动释放信号量资源。
    fn drop(&mut self) { self.s.release(); }
}
impl<'a> Deref for SemaGuard<'a> {
    type Target = Sema;
    /// 通过守卫访问底层信号量。
    fn deref(&self) -> &Self::Target {
        eprintln!("[DBG] Deref::deref");
        self.s }
}
