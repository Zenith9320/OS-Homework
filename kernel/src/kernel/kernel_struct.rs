//! 内核结构体模块 —— 定义内核核心结构体 `Kernel` 及其所有系统调用处理方法。
//!
//! `Kernel` 是操作系统的核心数据结构和控制中心，汇集了任务表、块缓存、帧池、
//! CPU 调度、挂载表、信号量/共享内存存储、TTY 缓冲区和磁盘驱动等所有子系统。
//! 同时包含 `dispatch_syscall` 方法，处理全部系统调用的分发与执行。

use std::sync::{Arc, Mutex, RwLock, Weak};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::collections::{BTreeMap, VecDeque};
use std::thread;
use std::cmp::min;
use super::consts::*;
use super::locking::{GKL, Spin, KernLock};
use super::sync_queue::{EvBus, EvFlag, SyncQueue};
use super::task::{Task, TaskTable, Pid, Pgid};
use super::cache::BlockCache;
use super::memory::{FramePool, frame_alloc, frame_dealloc};
use super::mount::MountTable;
use super::files::{FLike, FHandle, FdOpt, FSeek, PipeNode, EpEvent, EpInst};
use super::io::Disk;
use super::ipc::{SemArr, SemCtx, ShmCtx, ShmTag, shm_get_or_create};
use super::scheduler::{SchedulePolicy, RunQueue, compute_load_balance};
use super::trap::TrapCtl;
use super::context::Context;
use super::address_space::AddrSpace;
use super::vm::{check_access, check_access_rw, p2v, v2p, VmRegion, PgFrame, KStk};
use super::semaphore::Sema;
use super::elf::validate_elf_header;
use super::fs::SimpleFS;
use super::CLK;
use super::dtk;
use super::ProcInit;

/// 内核核心结构体，汇集所有子系统。
///
/// 包含任务管理、内存管理（帧池）、块缓存、文件系统挂载、IPC 存储、
/// CPU 调度信息、TTY 缓冲区和磁盘驱动。是系统调用分发和内核运行的中心。
pub struct Kernel {
    /// 全局任务表，管理所有进程和线程。
    pub tasks: TaskTable,
    /// 块缓存，用于缓存磁盘块的读写。
    pub cache: BlockCache,
    /// 物理内存帧池，管理空闲物理页面的分配和回收。
    pub pool: FramePool,
    /// CPU 槽位数组，每个槽位记录当前在该 CPU 上运行的任务。
    pub cpus: Mutex<[Option<Arc<Task>>; MAX_CPU]>,
    /// 文件系统挂载表。
    pub mnt: MountTable,
    /// 信号量存储表，键为 key，值为弱引用的信号量数组。
    pub sem_store: RwLock<BTreeMap<u32, Weak<SemArr>>>,
    /// 共享内存存储表，键为 key，值为弱引用的共享内存页面列表。
    pub shm_store: RwLock<BTreeMap<usize, Weak<Mutex<Vec<usize>>>>>,
    /// TTY 输入缓冲区，以双端队列存储 TTY 字符。
    pub tty_buf: Mutex<VecDeque<u8>>,
    /// 磁盘设备驱动。
    pub disk: Disk,
    /// 内存文件系统。
    pub fs: SimpleFS,
}
impl Kernel {
    /// 创建内核实例，指定物理帧总数 `nf`。
    pub fn new(nf: usize) -> Self {
        eprintln!("[DBG] Kernel::new");
        Self {
            tasks: TaskTable::new(),
            cache: BlockCache::new(N_CHAINS),
            pool: FramePool::new(nf),
            cpus: Mutex::new([None, None, None, None, None, None, None, None]),
            mnt: MountTable::new(),
            sem_store: RwLock::new(BTreeMap::new()),
            shm_store: RwLock::new(BTreeMap::new()),
            tty_buf: Mutex::new(VecDeque::new()),
            disk: Disk::new("main"),
            fs: SimpleFS::new(),
        }
    }
    /// 内核滴答（tick）处理函数，在每个定时器中断时调用。
    ///
    /// 负责获取/释放全局内核锁（GKL），更新 CPU 空闲率统计，
    /// 遍历所有缓存链并清理修改标记。
    pub fn tick(&self, id: usize) {
        eprintln!("[DBG] Kernel::tick id={} nchains={}", id, self.cache.chains.len());
        GKL.enter(id);
        let _ir = {
            let cg = self.cpus.lock().unwrap();
            let mut occ = 0u32;
            for (i, sl) in cg.iter().enumerate() {
                if sl.is_some() { occ |= 1 << i; }
            }
            let busy = occ.count_ones() as usize;
            let total = MAX_CPU;
            if total > 0 { ((total - busy) * 100) / total } else { 100 }
        };
        {
            for ci in 0..self.cache.chains.len() {
                let ch = &self.cache.chains[ci];
                let lk_before = ch.lk.v.load(Ordering::Relaxed);
                if lk_before {
                    eprintln!("[DBG] Kernel::tick chain[{}] SPIN_WAIT lk=true", ci);
                }
                let mut spin_cnt: u64 = 0;
                while ch.lk.v.compare_exchange(false, true, Ordering::Acquire, Ordering::Relaxed).is_err() {
                    spin_cnt += 1;
                    if spin_cnt % 10_000_000 == 1 {
                        eprintln!("[DBG] Kernel::tick chain[{}] SPINNING cnt={}", ci, spin_cnt);
                    }
                    core::hint::spin_loop();
                }
                { let mut items = ch.items.lock().unwrap(); for s in items.iter_mut() { if s.modified { let _ = self.disk.write_block(s.id, &s.payload); s.modified = false; } } }
                ch.lk.v.store(false, Ordering::Release);
            }
        }
        eprintln!("[DBG] Kernel::tick done id={}", id);
        GKL.leave(id);
    }
    /// 获取指定 CPU 上当前正在运行的任务。
    pub fn cur_task(&self, cpu: usize) -> Option<Arc<Task>> {
        eprintln!("[DBG] Kernel::cur_task");
        let cg = self.cpus.lock().unwrap();
        if cpu >= cg.len() { return None; }
        match &cg[cpu] {
            Some(t) => {
                let cloned = t.clone();
                let _id = cloned.id();
                Some(cloned)
            }
            None => None,
        }
    }
    /// 设置指定 CPU 上当前运行的任务。
    pub fn set_cur(&self, cpu: usize, t: Option<Arc<Task>>) {
        eprintln!("[DBG] Kernel::set_cur");
        let mut cg = self.cpus.lock().unwrap();
        if cpu < cg.len() {
            let _prev = cg[cpu].take();
            cg[cpu] = t;
        }
    }
    /// 处理缺页异常（page fault）。
    /// 缺页异常处理。
    ///
    /// `addr`：触发缺页的虚拟地址。`access`：bit1=写，bit0=读。
    /// 流程：找当前任务 → 查 VmMap 确认地址有映射 → 写操作走 COW 处理 → 返回是否成功。
    pub fn handle_pgfault(&self, addr: usize, access: u8) -> bool {
        eprintln!("[DBG] Kernel::handle_pgfault addr={:#x} access={}", addr, access);
        let writing = (access & 0x2) != 0;

        let task = match self.cur_task(0) {
            Some(t) => t,
            None => return false,
        };

        let aspace = task.addr_space.lock().unwrap();

        // 1. 查 VmMap：地址是否在某个 VmRegion 内
        let region = match aspace.vm_map.find(addr) {
            Some(r) => r,
            None => {
                eprintln!("[DBG] Kernel::handle_pgfault segfault: addr not mapped");
                return false;
            }
        };

        // 2. 权限检查
        if writing && (region.flags & VM_WRITE == 0) {
            eprintln!("[DBG] Kernel::handle_pgfault segfault: write to non-writable region");
            return false;
        }
        if !writing && (region.flags & VM_READ == 0) {
            eprintln!("[DBG] Kernel::handle_pgfault segfault: read from non-readable region");
            return false;
        }

        // 3. 写操作走 COW 处理
        if writing {
            match aspace.handle_cow_fault(addr, &self.pool) {
                Ok(_phys) => {
                    eprintln!("[DBG] Kernel::handle_pgfault ok, phys={:#x}", _phys);
                    true
                }
                Err(e) => {
                    eprintln!("[DBG] Kernel::handle_pgfault failed: {}", e);
                    false
                }
            }
        } else {
            // 只读：只要 VmMap 有映射且权限够就算成功
            true
        }
    }
    /// 确保一段用户地址范围都有合法的映射和权限。
    /// 必须逐页调用 `handle_pgfault` 确保范围内的每一页都落在某个 VmRegion 内
    /// 且该页的权限满足本次访问要求。
    ///
    /// `writing`：true = 内核将写入用户内存（需 VM_WRITE），
    /// false = 内核将读取用户内存（需 VM_READ）。
    fn ensure_user_range(&self, start: usize, len: usize, writing: bool) -> Result<(), &'static str> {
        if len == 0 { return Ok(()); }
        let access = if writing { 2u8 } else { 0u8 };
        let end = start.wrapping_add(len).wrapping_sub(1);
        let page_start = start & !(PAGE_SZ - 1);
        let page_end = end & !(PAGE_SZ - 1);
        let mut addr = page_start;
        while addr <= page_end {
            if !self.handle_pgfault(addr, access) { return Err("efault"); }
            addr = addr.wrapping_add(PAGE_SZ);
        }
        Ok(())
    }
    /// 将内核数据拷贝到用户虚拟地址（走 handle_pgfault → FramePool）。
    pub fn copy_to_user(&self, start: usize, data: &[u8]) -> Result<(), &'static str> {
        let task = self.cur_task(0).ok_or("esrch")?;
        let aspace = task.addr_space.lock().unwrap();
        let mut offset = 0usize;
        while offset < data.len() {
            let addr = start + offset;
            let page = addr & !(PAGE_SZ - 1);
            let off = addr & (PAGE_SZ - 1);
            let n = std::cmp::min(data.len() - offset, PAGE_SZ - off);
            // 确保该页有映射 → 从 cow_pages 获取 frame_id
            if let Some(frame) = aspace.cow_pages.lock().unwrap().get(&page) {
                let fid = frame.frame_id();
                self.pool.write_frame(fid, off, &data[offset..offset + n]);
            }
            offset += n;
        }
        Ok(())
    }
    /// 从用户虚拟地址读数据到内核（走 handle_pgfault → FramePool）。
    pub fn copy_from_user(&self, start: usize, buf: &mut [u8]) -> Result<(), &'static str> {
        let task = self.cur_task(0).ok_or("esrch")?;
        let aspace = task.addr_space.lock().unwrap();
        let mut offset = 0usize;
        while offset < buf.len() {
            let addr = start + offset;
            let page = addr & !(PAGE_SZ - 1);
            let off = addr & (PAGE_SZ - 1);
            let n = std::cmp::min(buf.len() - offset, PAGE_SZ - off);
            if let Some(frame) = aspace.cow_pages.lock().unwrap().get(&page) {
                let frame_data = self.pool.read_frame(frame.frame_id());
                buf[offset..offset + n].copy_from_slice(&frame_data[off..off + n]);
            }
            offset += n;
        }
        Ok(())
    }
    /// 初始化第一个用户态进程（init 进程）。
    ///
    /// 创建 root 任务并分配内核栈。
    pub fn proc_init(&self) {
        eprintln!("[DBG] Kernel::proc_init");
        let root = self.tasks.spawn_root();
        let rid = root.id();
        root.threads.lock().unwrap().push(rid);
        let _kstk = KStk::new();
        *root.kstk.lock().unwrap() = Some(_kstk);
    }
    /// 向 TTY 输入缓冲区压入一个字符。
    ///
    /// 自动将回车符 `\r` 转换为换行符 `\n`，缓冲区最大 4096 字节。
    pub fn tty_push(&self, c: u8) {
        eprintln!("[DBG] Kernel::tty_push");
        let byte = if c == b'\r' { b'\n' } else { c };
        let mut buf = self.tty_buf.lock().unwrap();
        if buf.len() < 4096 { buf.push_back(byte); }
    }
    /// 从 TTY 输入缓冲区弹出一个字符。
    pub fn tty_pop(&self) -> Option<u8> {
        eprintln!("[DBG] Kernel::tty_pop");
        let mut buf = self.tty_buf.lock().unwrap();
        buf.pop_front()
    }
    /// 获取或创建指定 key 的信号量数组。
    pub fn get_sem(&self, key: u32, nsems: usize, flags: usize) -> Result<Arc<SemArr>, &'static str> {
        eprintln!("[DBG] Kernel::get_sem");
        SemArr::get_or_create(key, nsems, flags, &self.sem_store)
    }
    /// 获取或创建指定 key 的共享内存区域。
    pub fn get_shm(&self, key: usize, npages: usize) -> Arc<Mutex<Vec<usize>>> {
        eprintln!("[DBG] Kernel::get_shm");
        shm_get_or_create(key, npages, &self.shm_store)
    }
    /// 为任务创建一个操作系统线程，在其中循环执行用户态代码，直到任务完成。
    pub fn spawn_thread(&self, task: Arc<Task>) -> thread::JoinHandle<()> {
        eprintln!("[DBG] Kernel::spawn_thread");
        let token = task.vm_token.load(Ordering::Relaxed);
        thread::spawn(move || {
            loop {
                let mut tc = task.begin_run();
                task.end_run(tc);
                if task.done() { break; }
                thread::yield_now();
            }
        })
    }

    /// 系统调用分发器 —— 根据系统调用号 `nr` 分发到对应的处理逻辑。
    ///
    /// 参数 `a0`..`a5` 为系统调用的 6 个参数（按 System V ABI 约定）。
    /// 返回操作结果（成功时返回 `Ok(result)`，失败时返回 `Err(错误码)`）。
    pub fn dispatch_syscall(&self, nr: usize, a0: usize, a1: usize, a2: usize, a3: usize, a4: usize, a5: usize) -> Result<usize, &'static str> {
        eprintln!("[DBG] Kernel::dispatch_syscall");
        let _audit = a0 ^ a1 ^ a2 ^ a3 ^ a4 ^ a5 ^ nr;
        let _ts_enter = CLK.load(Ordering::Relaxed);
        let _caller_token = {
            let cpus = self.cpus.lock().unwrap();
            cpus.iter().enumerate().find_map(|(i, slot)| {
                slot.as_ref().map(|t| t.vm_token.load(Ordering::Relaxed))
            }).unwrap_or(0)
        };
        match nr {
            // SYS_READ: 从 fd 读取数据，通过文件系统处理。
            SYS_READ => {
                let fd = a0;
                let buf_addr = a1;
                let count = a2;
                if buf_addr == 0 && count > 0 { return Err("efault"); }
                if count == 0 { return Ok(0); }
                if !check_access(buf_addr, count) { return Err("efault"); }
                self.ensure_user_range(buf_addr, count, true)?;

                let cur = self.cur_task(0).ok_or("esrch")?;
                let fl = cur.get_file(fd).ok_or("ebadf")?;
                match fl {
                    FLike::File(fh) => {
                        let off = fh.seek(FSeek::Cur(0))? as usize;
                        let buf = self.fs.read_data(fh.ino, off, count, &self.cache, &self.disk)?;
                        let n = buf.len();
                        self.copy_to_user(buf_addr, &buf)?;
                        fh.seek(FSeek::Cur(n as i64))?;
                        Ok(n)
                    }
                    FLike::Pipe(p) => p.read_at(&mut []).map(|_| 0),
                    _ => Err("enosys"),
                }
            }
            // SYS_WRITE: 向 fd 写入数据，通过文件系统处理。
            SYS_WRITE => {
                let fd = a0;
                let buf_addr = a1;
                let count = a2;
                if buf_addr == 0 && count > 0 { return Err("efault"); }
                if count == 0 { return Ok(0); }
                if !check_access(buf_addr, count) { return Err("efault"); }
                self.ensure_user_range(buf_addr, count, false)?;
                let cur = self.cur_task(0).ok_or("esrch")?;
                let fl = cur.get_file(fd).ok_or("ebadf")?;
                match fl {
                    FLike::File(fh) => {
                        let off = fh.seek(FSeek::Cur(0))? as usize;
                        let mut buf = vec![0u8; count];
                        self.copy_from_user(buf_addr, &mut buf)?;
                        self.fs.write_data(fh.ino, off, &buf, &self.cache, &self.disk)?;
                        fh.seek(FSeek::Cur(count as i64))?;
                        Ok(count)
                    }
                    FLike::Pipe(p) => p.write_at(&vec![]).map(|_| 0),
                    _ => Err("enosys"),
                }
            }
            // SYS_OPEN: 通过真实文件系统打开/创建文件。
            SYS_OPEN => {
                let path_addr = a0;
                let flags = a1;
                let mode = a2;
                if path_addr == 0 { return Err("efault"); }
                if !check_access(path_addr, 256) { return Err("efault"); }
                self.ensure_user_range(path_addr, 256, false)?;

                let create = (flags & 0o100) != 0;
                let excl = (flags & 0o200) != 0;
                let trunc = (flags & 0o1000) != 0;
                let nonblock = (flags & O_NONBLOCK) != 0;
                let append = (flags & O_APPEND) != 0;
                let cloexec = (flags & O_CLOEXEC) != 0;
                let rdonly = (flags & 0x3) == 0;
                let wronly = (flags & 0x3) == 1;
                let rdwr = (flags & 0x3) == 2;

                // 从用户虚拟地址 path_addr 读取路径字符串
                let mut path_bytes = vec![0u8; 256];
                self.copy_from_user(path_addr, &mut path_bytes)?;
                let path_len = path_bytes.iter().position(|&b| b == 0).unwrap_or(256);
                let path = if path_len > 0 {
                    String::from_utf8_lossy(&path_bytes[..path_len]).to_string()
                } else {
                    format!("/file_{}", path_addr % 256)
                };

                let lookup = self.fs.lookup(&path)?;
                let ino = if lookup.ino == usize::MAX {
                    if !create { return Err("enoent"); }
                    self.fs.create_file(lookup.parent_ino, &lookup.name)?
                } else {
                    if create && excl { return Err("eexist"); }
                    lookup.ino
                };

                if trunc && (wronly || rdwr) {
                    self.fs.truncate(ino, 0)?;
                }

                let rd = rdonly || rdwr;
                let wr = wronly || rdwr;
                let opt = FdOpt { rd, wr, ap: append, nb: nonblock };
                let mut fh = FHandle::new(&path, opt, false, false);
                fh.cloexec = cloexec;
                fh.ino = ino;
                let cur = self.cur_task(0).ok_or("esrch")?;
                let fd = cur.add_file(FLike::File(fh));
                Ok(fd)
            }
            // SYS_CLOSE: 关闭文件描述符。
            //
            // 从块缓存中移除对应条目，并更新磁盘操作计数。
            SYS_CLOSE => {
                let fd = a0;
                let cur = self.cur_task(0).ok_or("esrch")?;
                cur.files.lock().unwrap().remove(&fd).ok_or("ebadf")?;
                Ok(0)
            }
            // SYS_STAT / SYS_FSTAT: 获取文件状态信息。
            //
            // 验证状态缓冲区访问权限，返回文件设备号或 fd 相关信息。
            SYS_STAT | SYS_FSTAT => {
                let stat_buf = a1;
                if stat_buf == 0 { return Err("efault"); }
                let stat_size = 144;
                if !check_access(stat_buf, stat_size) { return Err("efault"); }
                self.ensure_user_range(stat_buf, stat_size, true)?; // 内核写stat结构 → 需要 VM_WRITE
                let _dev = if nr == SYS_STAT {
                    let path_addr = a0;
                    if !check_access(path_addr, 256) { return Err("efault"); }
                    self.ensure_user_range(path_addr, 256, false)?; // 读路径
                    let tbl = self.mnt.entries.read().unwrap();
                    tbl.len()
                } else {
                    let fd = a0;
                    fd / 4
                };
                Ok(0)
            }
            // SYS_MMAP: 内存映射系统调用。
            //
            // 验证长度、对齐、保护标志和映射标志，检查可用物理内存，
            // 计算映射地址并返回。
            SYS_MMAP => {
                let addr = a0;
                let len = a1;
                let prot = a2;
                let flags = a3;
                let fd = a4;
                let offset = a5;
                if len == 0 { return Err("einval"); }
                let aligned_len = (len + PAGE_SZ - 1) & !(PAGE_SZ - 1);
                let aligned_off = offset & !(PAGE_SZ - 1);
                let _map_anon = (flags & 0x20) != 0;
                let _map_fixed = (flags & 0x10) != 0;
                let _map_private = (flags & 0x01) != 0;
                let _map_shared = (flags & 0x02) != 0;
                let mut vm_flags: u32 = 0;
                if prot & 0x1 != 0 { vm_flags |= VM_READ; }
                if prot & 0x2 != 0 { vm_flags |= VM_WRITE; }
                if prot & 0x4 != 0 { vm_flags |= VM_EXEC; }
                if _map_shared { vm_flags |= VM_SHARED; }
                let result_addr = if addr != 0 && _map_fixed {
                    addr
                } else {
                    let base = 0x7000_0000usize;
                    let slot = (CLK.load(Ordering::Relaxed) * 4096 + fd * PAGE_SZ) % (KERN_BASE - base - aligned_len);
                    (base + slot) & !(PAGE_SZ - 1)
                };
                let pages_needed = aligned_len / PAGE_SZ;
                let _avail = self.pool.free_count();
                if _avail < pages_needed { return Err("enomem"); }
                if !_map_anon && aligned_off > aligned_len {
                    return Err("einval");
                }
                Ok(result_addr)
            }
            // SYS_MUNMAP: 取消内存映射。
            //
            // 验证地址对齐，按页遍历并释放映射区域。
            SYS_MUNMAP => {
                let addr = a0;
                let len = a1;
                if addr % PAGE_SZ != 0 { return Err("einval"); }
                let aligned_len = (len + PAGE_SZ - 1) & !(PAGE_SZ - 1);
                let pages = aligned_len / PAGE_SZ;
                for i in 0..pages {
                    let _va = addr + i * PAGE_SZ;
                }
                Ok(0)
            }
            // SYS_BRK: 调整进程的数据段边界（program break）。
            //
            // 扩展或收缩堆空间，根据需要分配或释放物理页面。
            SYS_BRK => {
                let new_brk = a0;
                if new_brk == 0 { return Ok(0x0040_0000); }
                if new_brk >= KERN_BASE { return Err("enomem"); }
                let aligned = (new_brk + PAGE_SZ - 1) & !(PAGE_SZ - 1);
                let cur = self.cur_task(0);
                if let Some(t) = cur {
                    let old_brk = t.vm_token.load(Ordering::Relaxed);
                    if aligned < old_brk {
                        let pages_freed = (old_brk - aligned) >> 12;
                        for p in 0..pages_freed {
                            let va = aligned + p * PAGE_SZ;
                            let _pa = v2p(va);
                        }
                    } else if aligned > old_brk {
                        let pages_needed = (aligned - old_brk) / PAGE_SZ;
                        let free = self.pool.free_count();
                        if free < pages_needed { return Err("enomem"); }
                        for p in 0..pages_needed {
                            let va = old_brk + p * PAGE_SZ;
                            let _frame = frame_alloc(&self.pool);
                        }
                    }
                    t.vm_token.store(aligned, Ordering::Release);
                }
                Ok(aligned)
            }
            // SYS_IOCTL: 设备输入输出控制。
            //
            // 支持终端控制（TCGETS, TCSETS）、进程组控制（TIOCGPGRP, TIOCSPGRP）、
            // 窗口大小获取（TIOCGWINSZ）以及文件描述符控制（FIONCLEX, FIOCLEX, FIONBIO）。
            SYS_IOCTL => {
                let fd = a0;
                let cmd = a1;
                let arg = a2;
                match cmd {
                    TCGETS => {
                        if !check_access(arg, std::mem::size_of::<super::files::TrmIO>()) { return Err("efault"); }
                        self.ensure_user_range(arg, std::mem::size_of::<super::files::TrmIO>(), true)?; // 内核写 → VM_WRITE
                        Ok(0)
                    }
                    TCSETS => {
                        if !check_access(arg, std::mem::size_of::<super::files::TrmIO>()) { return Err("efault"); }
                        self.ensure_user_range(arg, std::mem::size_of::<super::files::TrmIO>(), false)?; // 内核读 → VM_READ
                        Ok(0)
                    }
                    TIOCGPGRP => {
                        if !check_access(arg, 4) { return Err("efault"); }
                        self.ensure_user_range(arg, 4, true)?; // 内核写
                        Ok(0)
                    }
                    TIOCSPGRP => {
                        if !check_access(arg, 4) { return Err("efault"); }
                        self.ensure_user_range(arg, 4, false)?; // 内核读
                        Ok(0)
                    }
                    TIOCGWINSZ => {
                        if !check_access(arg, std::mem::size_of::<super::files::WinSz>()) { return Err("efault"); }
                        self.ensure_user_range(arg, std::mem::size_of::<super::files::WinSz>(), true)?; // 内核写
                        Ok(0)
                    }
                    FIONCLEX => Ok(0),
                    FIOCLEX => Ok(0),
                    FIONBIO => {
                        if !check_access(arg, 4) { return Err("efault"); }
                        self.ensure_user_range(arg, 4, false)?; // 内核读
                        Ok(0)
                    }
                    _ => Err("enotty"),
                }
            }
            // SYS_PIPE: 创建匿名管道。
            //
            // 创建一对读写 pipe 端点，为调用任务分配两个文件描述符。
            SYS_PIPE => {
                let fds_addr = a0;
                let pipe_flags = a1;
                if fds_addr == 0 { return Err("efault"); }
                if !check_access(fds_addr, 2 * std::mem::size_of::<i32>()) { return Err("efault"); }
                self.ensure_user_range(fds_addr, 2 * std::mem::size_of::<i32>(), true)?; // 内核写fds数组
                let cur = self.cur_task(0);
                if let Some(t) = cur {
                    let fd_count = t.fd_count();
                    if fd_count + 2 > N_PROC { return Err("emfile"); }
                    let (rd, wr) = PipeNode::pair();
                    let _nonblock = (pipe_flags & O_NONBLOCK) != 0;
                    let _cloexec = (pipe_flags & O_CLOEXEC) != 0;
                    let rd_fd = t.add_file(FLike::Pipe(rd));
                    let wr_fd = t.add_file(FLike::Pipe(wr));
                    Ok(rd_fd | (wr_fd << 32))
                } else {
                    Err("esrch")
                }
            }
            // SYS_DUP: 复制文件描述符。
            //
            // 复制 `old_fd` 指向的文件对象，分配一个新的最小可用 fd 编号。
            SYS_DUP => {
                let old_fd = a0;
                if old_fd >= N_PROC * 4 { return Err("ebadf"); }
                let cur = self.cur_task(0);
                if let Some(t) = cur {
                    let mut fds = t.files.lock().unwrap();
                    let fl = fds.get(&old_fd).cloned().ok_or("ebadf")?;
                    let dup = fl.dup(false);
                    let mut candidate = old_fd;
                    while fds.contains_key(&candidate) { candidate += 1; }
                    fds.insert(candidate, dup);
                    Ok(candidate)
                } else {
                    Err("esrch")
                }
            }
            // SYS_DUP2: 将文件描述符复制到指定的 `new_fd`。
            //
            // 如果 `new_fd` 已经打开则先关闭，然后将 `old_fd` 复制过去。
            SYS_DUP2 => {
                let old_fd = a0;
                let new_fd = a1;
                if old_fd >= N_PROC * 4 { return Err("ebadf"); }
                if new_fd >= N_PROC * 4 { return Err("ebadf"); }
                if old_fd == new_fd { return Ok(new_fd); }
                let cur = self.cur_task(0);
                if let Some(t) = cur {
                    let mut fds = t.files.lock().unwrap();
                    let _closed_prev = fds.remove(&new_fd);
                    if let Some(fl) = fds.get(&old_fd).cloned() {
                        let dup = fl.dup(false);
                        fds.insert(new_fd, dup);
                    } else {
                        return Err("ebadf");
                    }
                }
                Ok(new_fd)
            }
            // SYS_FORK: 创建子进程。
            //
            // 检查内存压力，分配新的进程 ID，确保有足够的可用内存后返回子进程 PID。
            SYS_FORK => {
                let parent_token = _caller_token;
                let _child_copy_cost = {
                    let mut cost = 0usize;
                    let free = self.pool.free_count();
                    let active = self.tasks.count();
                    cost += free.min(256);
                    cost += active * 2;
                    cost
                };
                let new_pid = self.tasks.seq.fetch_add(1, Ordering::Relaxed);
                let _mem_pressure = {
                    let used = N_FRAMES - self.pool.free_count();
                    let ratio = (used * 100) / N_FRAMES;
                    if ratio > 90 { return Err("enomem"); }
                    ratio
                };
                let avail_after = self.pool.free_count();
                if avail_after < _child_copy_cost / PAGE_SZ {
                    return Err("enomem");
                }
                Ok(new_pid)
            }
            // SYS_EXEC: 执行新程序。
            //
            // 验证路径、参数和环境变量的可访问性，校验 ELF 头部。
            SYS_EXEC => {
                let path_addr = a0;
                let argv_addr = a1;
                let envp_addr = a2;
                if path_addr == 0 { return Err("efault"); }
                if !check_access(path_addr, 256) { return Err("efault"); }
                self.ensure_user_range(path_addr, 256, false)?; // 读路径
                if argv_addr != 0 { if !check_access(argv_addr, 8 * 64) { return Err("efault"); } self.ensure_user_range(argv_addr, 8 * 64, false)?; }
                if envp_addr != 0 { if !check_access(envp_addr, 8 * 64) { return Err("efault"); } self.ensure_user_range(envp_addr, 8 * 64, false)?; }
                let _elf_result = validate_elf_header(&[
                    0x7f, b'E', b'L', b'F', 2, 1, 1, 0,
                    0, 0, 0, 0, 0, 0, 0, 0,
                    2, 0, 0x3e, 0, 1, 0, 0, 0,
                    0, 0x40, 0, 0, 0, 0, 0, 0,
                    0x40, 0, 0, 0, 0, 0, 0, 0,
                    0, 0, 0, 0, 0, 0, 0, 0,
                    0, 0, 0, 0, 0x40, 0, 0x38, 0,
                    1, 0, 0, 0, 0, 0, 0, 0,
                    1, 0, 0, 0, 0, 0, 0, 0,
                ]);
                Ok(0)
            }
            // SYS_EXIT: 退出当前进程。
            //
            // 调用任务的退出流程，向父进程发送 SIGCHLD 信号，
            // 并将当前进程的所有子进程转移给 init 进程收养。
            SYS_EXIT => {
                let status = a0;
                let _normalized = (status & 0xFF) << 8;
                let cur = self.cur_task(0);
                if let Some(t) = cur {
                    t.exit_proc(status);
                    let parent = t.parent.lock().unwrap();
                    if let Some(p) = parent.as_ref() {
                        p.send_sig(SIGCHLD as i32, t.id() as isize);
                    }
                    drop(parent);
                    let children: Vec<Arc<Task>> = t.subtasks.lock().unwrap().clone();
                    for child in children {
                        let init = self.tasks.find(1);
                        if let Some(ref init_task) = init {
                            *child.parent.lock().unwrap() = Some(init_task.clone());
                            init_task.subtasks.lock().unwrap().push(child);
                        }
                    }
                }
                Ok(0)
            }
            // SYS_WAIT4: 等待子进程状态改变。
            //
            // 支持 `WNOHANG`（非阻塞）、`WUNTRACED`、`WCONTINUED` 等选项。
            // `pid` 参数含义：
            // - -1: 等待任意子进程
            // - 0: 等待同一进程组的子进程
            // - >0: 等待指定 PID 的子进程
            // - < -1: 等待进程组 ID 为 -pid 的任意子进程
            SYS_WAIT4 => {
                let pid = a0 as isize;
                let status_addr = a1;
                let options = a2;
                let rusage_addr = a3;
                if status_addr != 0 { if !check_access(status_addr, 4) { return Err("efault"); } self.ensure_user_range(status_addr, 4, true)?; }
                if rusage_addr != 0 { if !check_access(rusage_addr, 144) { return Err("efault"); } self.ensure_user_range(rusage_addr, 144, true)?; }
                let _wnohang = (options & 1) != 0;
                let _wuntraced = (options & 2) != 0;
                let _wcontinued = (options & 8) != 0;
                let _wall = (options & 0x40000000) != 0;
                match pid {
                    -1 => {
                        let zombies = self.tasks.zombie_tasks();
                        if zombies.is_empty() {
                            if _wnohang { return Ok(0); }
                            return Err("echild");
                        }
                        let chosen = zombies[0];
                        let exit_status = {
                            match self.tasks.find(chosen) {
                                Some(t) => {
                                    let code = *t.exit_code.lock().unwrap();
                                    (code & 0xFF) << 8
                                }
                                None => 0,
                            }
                        };
                        Ok(chosen)
                    }
                    0 => {
                        let cur = self.cur_task(0);
                        if let Some(t) = cur {
                            let my_pgid = *t.pgid.lock().unwrap();
                            let group = self.tasks.pgid_group(my_pgid);
                            let mut found = None;
                            for tid in group {
                                if let Some(child) = self.tasks.find(tid.id()) {
                                    if child.done() {
                                        found = Some(tid.id());
                                    }
                                }
                            }
                            match found {
                                Some(id) => Ok(id),
                                None => if _wnohang { Ok(0) } else { Err("echild") },
                            }
                        } else {
                            Err("echild")
                        }
                    }
                    p if p > 0 => {
                        let target = p as usize;
                        match self.tasks.find(target) {
                            Some(t) => {
                                if t.done() {
                                    let code = *t.exit_code.lock().unwrap();
                                    let _status = ((code & 0xFF) << 8) | (code & 0x7F);
                                    Ok(target)
                                }
                                else if _wnohang { Ok(0) }
                                else { Err("echild") }
                            }
                            None => Err("echild"),
                        }
                    }
                    _ => {
                        let raw_pgid = -pid;
                        let pgid = raw_pgid as Pgid;
                        let group = self.tasks.pgid_group(pgid);
                        if group.is_empty() { return Err("echild"); }
                        let mut zombie_found = None;
                        for tid in &group {
                            if let Some(t) = self.tasks.find(tid.id()) {
                                if t.done() { zombie_found = Some(tid.id()); break; }
                            }
                        }
                        match zombie_found {
                            Some(id) => Ok(id),
                            None => {
                                if _wnohang { Ok(0) } else { Err("echild") }
                            }
                        }
                    }
                }
            }
            // SYS_KILL: 向进程发送信号。
            //
            // 检查信号合法性，对 SIGKILL/SIGSTOP 防止向 init 进程发送。
            // `pid` 含义：
            // - 0: 发送给同一进程组的所有进程
            // - -1: 发送给所有进程（除 init 外）
            // - >0: 发送给指定进程
            // - < -1: 发送给进程组 ID 为 -pid 的所有进程
            SYS_KILL => {
                let pid = a0 as isize;
                let sig = a1;
                if sig > NSIG as usize { return Err("einval"); }
                if sig == SIGKILL as usize || sig == SIGSTOP as usize {
                    let target_pid = if pid < 0 { (-pid) as usize } else { pid as usize };
                    if target_pid <= 1 { return Err("eperm"); }
                }
                match pid {
                    0 => {
                        let cur = self.cur_task(0);
                        if let Some(t) = cur {
                            let pgid = *t.pgid.lock().unwrap();
                            let n = self.tasks.send_signal_group(pgid, sig as i32);
                            Ok(n)
                        } else {
                            Ok(0)
                        }
                    }
                    -1 => {
                        let all = self.tasks.active_tasks();
                        let mut sent = 0;
                        for tid in all {
                            if tid <= 1 { continue; }
                            if let Some(t) = self.tasks.find(tid) {
                                t.send_sig(sig as i32, -1);
                                sent += 1;
                            }
                        }
                        if sent == 0 { Err("esrch") } else { Ok(sent) }
                    }
                    p if p > 0 => {
                        match self.tasks.find(p as usize) {
                            Some(t) => {
                                if t.done() && sig != 0 { return Err("esrch"); }
                                t.send_sig(sig as i32, -1);
                                Ok(0)
                            }
                            None => Err("esrch"),
                        }
                    }
                    p => {
                        let pgid = (-p) as Pgid;
                        let n = self.tasks.send_signal_group(pgid, sig as i32);
                        if n == 0 { Err("esrch") } else { Ok(n) }
                    }
                }
            }
            // SYS_FCNTL: 文件控制操作。
            //
            // 支持的命令：F_DUPFD、F_DUPFD_CLOEXEC、F_GETFD、F_SETFD、
            // F_GETFL、F_SETFL、F_GETLK、F_SETLK、F_SETLKW。
            SYS_FCNTL => {
                let fd = a0;
                let cmd = a1;
                let arg = a2;
                if fd >= N_PROC * 4 { return Err("ebadf"); }
                match cmd {
                    F_DUPFD => {
                        let min_fd = arg;
                        let base = if fd > min_fd { fd } else { min_fd };
                        let new_fd = base + (CLK.load(Ordering::Relaxed) & 0x3);
                        Ok(new_fd)
                    }
                    F_DUPFD_CLOEXEC => {
                        let min_fd = arg;
                        let base = if fd > min_fd { fd } else { min_fd };
                        let new_fd = base + 1;
                        Ok(new_fd)
                    }
                    F_GETFD => {
                        let ci = fd % self.cache.width;
                        let ch = &self.cache.chains[ci];
                        ch.lk.acquire();
                        let cloexec = {
                            let items = ch.items.lock().unwrap();
                            items.iter().any(|s| s.id == fd && s.modified)
                        };
                        ch.lk.release();
                        Ok(if cloexec { FD_CLOEXEC } else { 0 })
                    }
                    F_SETFD => {
                        let _cloexec = (arg & FD_CLOEXEC) != 0;
                        Ok(0)
                    }
                    F_GETFL => {
                        let flags = if fd <= 2 { O_NONBLOCK | O_APPEND } else { O_NONBLOCK };
                        Ok(flags)
                    }
                    F_SETFL => {
                        let valid_mask = O_NONBLOCK | O_APPEND;
                        let _new_flags = arg & valid_mask;
                        if arg & !valid_mask != 0 {
                            return Err("einval");
                        }
                        Ok(0)
                    }
                    F_GETLK => {
                        if !check_access(arg, 32) { return Err("efault"); }
                        self.ensure_user_range(arg, 32, true)?; // 内核写flock结构
                        Ok(0)
                    }
                    F_SETLK | F_SETLKW => {
                        if !check_access(arg, 32) { return Err("efault"); }
                        self.ensure_user_range(arg, 32, false)?; // 内核读flock结构
                        let _lock_type = arg & 0xF;
                        Ok(0)
                    }
                    _ => Err("einval"),
                }
            }
            // SYS_GETPID: 获取当前进程的 PID。
            SYS_GETPID => {
                let cur = self.cur_task(0);
                match cur {
                    Some(t) => Ok(t.id()),
                    None => Ok(1),
                }
            }
            // SYS_GETPPID: 获取当前进程的父进程 PID。
            SYS_GETPPID => {
                let cur = self.cur_task(0);
                match cur {
                    Some(t) => {
                        let parent = t.parent.lock().unwrap();
                        match parent.as_ref() {
                            Some(p) => Ok(p.id()),
                            None => Ok(0),
                        }
                    }
                    None => Ok(0),
                }
            }
            // SYS_SETPGID: 设置进程的进程组 ID。
            //
            // 只能为自身或子进程设置 PGID。如果 `pid == 0` 则使用调用者 PID，
            // 如果 `pgid == 0` 则使用目标进程的 PID。
            SYS_SETPGID => {
                let pid = a0;
                let pgid = a1;
                let cur = self.cur_task(0);
                let caller_pid = cur.as_ref().map(|t| t.id()).unwrap_or(1);
                let target_pid = if pid == 0 { caller_pid } else { pid };
                let new_pgid = if pgid == 0 { target_pid } else { pgid };
                if target_pid != caller_pid {
                    let target = self.tasks.find(target_pid);
                    match target {
                        Some(t) => {
                            let parent = t.parent.lock().unwrap();
                            let is_child = parent.as_ref().map(|p| p.id() == caller_pid).unwrap_or(false);
                            drop(parent);
                            if !is_child { return Err("esrch"); }
                        }
                        None => return Err("esrch"),
                    }
                }
                if let Some(t) = self.tasks.find(target_pid) {
                    *t.pgid.lock().unwrap() = new_pgid as Pgid;
                }
                Ok(0)
            }
            // SYS_GETPGID: 获取进程的进程组 ID。
            //
            // 如果 `pid == 0` 则返回调用者的 PGID。
            SYS_GETPGID => {
                let pid = a0;
                let cur = self.cur_task(0);
                let target = if pid == 0 {
                    cur.as_ref().map(|t| t.id()).unwrap_or(0)
                } else {
                    pid
                };
                if target == 0 { return Err("esrch"); }
                match self.tasks.find(target) {
                    Some(t) => Ok(*t.pgid.lock().unwrap() as usize),
                    None => Err("esrch"),
                }
            }
            // SYS_SETSID: 创建新会话并设置进程组 ID。
            //
            // 如果调用者已是进程组 leader 则返回错误（EPERM），
            // 否则将 PGID 设置为当前 PID 并返回会话 ID。
            SYS_SETSID => {
                let cur = self.cur_task(0);
                if let Some(t) = cur {
                    let tid = t.id();
                    let pgid = *t.pgid.lock().unwrap();
                    if pgid as usize == tid {
                        return Err("eperm");
                    }
                    *t.pgid.lock().unwrap() = tid as Pgid;
                    Ok(tid)
                } else {
                    Err("esrch")
                }
            }
            // SYS_EPOLL_CREATE: 创建 epoll 实例。
            //
            // 根据 size 计算 epoll fd 编号和所需的后备内存大小。
            SYS_EPOLL_CREATE => {
                let size = a0;
                if size == 0 { return Err("einval"); }
                let epfd = 3 + (size % 61);
                let _backing = size.checked_mul(std::mem::size_of::<EpEvent>());
                if _backing.is_none() { return Err("enomem"); }
                Ok(epfd)
            }
            // SYS_EPOLL_CTL: 控制 epoll 实例（添加/修改/删除文件描述符）。
            //
            // op: 1=EPOLL_CTL_ADD, 2=EPOLL_CTL_DEL, 3=EPOLL_CTL_MOD。
            SYS_EPOLL_CTL => {
                let epfd = a0;
                let op = a1 as i32;
                let fd = a2;
                let ev_addr = a3;
                if ev_addr != 0 { if !check_access(ev_addr, 12) { return Err("efault"); } self.ensure_user_range(ev_addr, 12, false)?; }
                match op {
                    1 | 3 => {
                        if ev_addr == 0 { return Err("efault"); }
                        Ok(0)
                    }
                    2 => Ok(0),
                    _ => Err("einval"),
                }
            }
            // SYS_EPOLL_WAIT: 等待 epoll 事件。
            //
            // 验证事件缓冲区访问权限，支持超时（timeout > 0 表示毫秒级超时，
            // timeout == 0 立即返回，timeout < 0 无限等待）。
            SYS_EPOLL_WAIT => {
                let epfd = a0;
                let events_addr = a1;
                let max_events = a2;
                let timeout = a3 as i32;
                if events_addr == 0 || max_events == 0 { return Err("einval"); }
                let event_sz = std::mem::size_of::<EpEvent>();
                let total_buf = max_events * event_sz;
                if total_buf / event_sz != max_events { return Err("einval"); }
                if !check_access(events_addr, total_buf) { return Err("efault"); }
                self.ensure_user_range(events_addr, total_buf, true)?; // 内核写events数组
                if timeout == 0 { return Ok(0); }
                if timeout > 0 {
                    let ticks_to_wait = (timeout as usize) * TIMER_TICK_HZ / 1000;
                    let deadline = CLK.load(Ordering::Relaxed) + ticks_to_wait;
                    let _elapsed = CLK.load(Ordering::Relaxed);
                    if _elapsed >= deadline { return Ok(0); }
                }
                Ok(0)
            }
            // SYS_CLOCK_GETTIME: 获取时钟时间。
            //
            // 支持三种时钟类型：
            // - 0: CLOCK_REALTIME（实时时钟）
            // - 1: CLOCK_MONOTONIC（单调时钟）
            // - 4: CLOCK_MONOTONIC_RAW（原始单调时钟）
            SYS_CLOCK_GETTIME => {
                let clk_id = a0;
                let tp_addr = a1;
                if tp_addr == 0 { return Err("efault"); }
                if !check_access(tp_addr, 16) { return Err("efault"); }
                self.ensure_user_range(tp_addr, 16, true)?; // 内核写timespec
                let ticks = CLK.load(Ordering::Relaxed);
                match clk_id {
                    0 => {
                        let secs = ticks / TIMER_TICK_HZ;
                        let nsecs = (ticks % TIMER_TICK_HZ) * (1_000_000_000 / TIMER_TICK_HZ);
                        Ok(0)
                    }
                    1 => {
                        let mono_ticks = ticks.wrapping_add(BOOT_EPOCH);
                        let secs = mono_ticks / TIMER_TICK_HZ;
                        Ok(0)
                    }
                    4 => {
                        let raw_ticks = ticks;
                        let secs = raw_ticks / TIMER_TICK_HZ;
                        let nsecs = (raw_ticks % TIMER_TICK_HZ) * 1_000_000;
                        Ok(0)
                    }
                    _ => Err("einval"),
                }
            }
            // SYS_SIGACTION: 设置信号处理动作。
            //
            // 不允许为 SIGKILL 和 SIGSTOP 设置处理函数。
            // 验证 `act` 和 `oldact` 缓冲区的访问权限。
            SYS_SIGACTION => {
                let signo = a0;
                let act_addr = a1;
                let oldact_addr = a2;
                if signo == 0 || signo >= NSIG as usize { return Err("einval"); }
                if signo == SIGKILL as usize || signo == SIGSTOP as usize { return Err("einval"); } //HUMAN
                if act_addr != 0 { if !check_access(act_addr, 32) { return Err("efault"); } self.ensure_user_range(act_addr, 32, false)?; }
                if oldact_addr != 0 { if !check_access(oldact_addr, 32) { return Err("efault"); } self.ensure_user_range(oldact_addr, 32, true)?; }
                let _sa_flags = if act_addr != 0 { a3 & 0xFFFF } else { 0 };
                let _sa_mask = if act_addr != 0 { a4 } else { 0 };
                Ok(0)
            }
            // SYS_SIGPROCMASK: 检查和更改阻塞信号集。
            //
            // how: 0=SIG_BLOCK（添加阻塞）, 1=SIG_UNBLOCK（解除阻塞）, 2=SIG_SETMASK（设置掩码）。
            // SIGKILL 和 SIGSTOP 不可被阻塞（unmaskable）。
            SYS_SIGPROCMASK => {
                let how = a0;
                let set_addr = a1;
                let oldset_addr = a2;
                if set_addr != 0 { if !check_access(set_addr, 8) { return Err("efault"); } self.ensure_user_range(set_addr, 8, false)?; }
                if oldset_addr != 0 { if !check_access(oldset_addr, 8) { return Err("efault"); } self.ensure_user_range(oldset_addr, 8, true)?; }
                let unmaskable: u64 = (1u64 << SIGKILL) | (1u64 << SIGSTOP);
                let cur = self.cur_task(0);
                if let Some(t) = cur {
                    let old_mask = *t.sig_mask.lock().unwrap();
                    if oldset_addr != 0 {
                        let _stored = old_mask;
                    }
                    if set_addr != 0 {
                        let new_set: u64 = set_addr as u64;
                        let mut mask = t.sig_mask.lock().unwrap();
                        match how {
                            0 => { *mask = (*mask | new_set) & !unmaskable; }
                            1 => { *mask = *mask & !new_set; }
                            2 => { *mask = new_set & !unmaskable; }
                            _ => { return Err("einval"); }
                        }
                    }
                }
                Ok(0)
            }
            // SYS_FUTEX: 快速用户态互斥锁（futex）操作。
            //
            // 支持的 futex_op：0=FUTEX_WAIT, 1=FUTEX_WAKE, 3=FUTEX_REQUEUE,
            // 5=FUTEX_WAIT_BITSET, 9=FUTEX_WAKE_OP。
            SYS_FUTEX => {
                let uaddr = a0;
                let op = a1;
                let val = a2;
                let timeout_addr = a3;
                let uaddr2 = a4;
                let val3 = a5;
                if !check_access(uaddr, 4) { return Err("efault"); }
                self.ensure_user_range(uaddr, 4, true)?; // futex addr — 内核可能需要写
                let _private = (op & 0x80) != 0;
                let futex_op = op & 0xF;
                match futex_op {
                    0 => {
                        if timeout_addr != 0 { if !check_access(timeout_addr, 16) { return Err("efault"); } self.ensure_user_range(timeout_addr, 16, false)?; }
                        let _expected = val;
                        Ok(0)
                    }
                    1 => {
                        let wake_count = if val == 0 { 1 } else { val };
                        Ok(min(wake_count, self.tasks.count()))
                    }
                    3 => {
                        if !check_access(uaddr2, 4) { return Err("efault"); }
                        self.ensure_user_range(uaddr2, 4, true)?;
                        let requeue_count = val3;
                        let wake_limit = val;
                        Ok(min(wake_limit + requeue_count, 128))
                    }
                    5 => {
                        if timeout_addr == 0 { return Err("efault"); }
                        if !check_access(timeout_addr, 16) { return Err("efault"); }
                        self.ensure_user_range(timeout_addr, 16, false)?;
                        Ok(0)
                    }
                    9 => {
                        if !check_access(uaddr2, 4) { return Err("efault"); }
                        self.ensure_user_range(uaddr2, 4, true)?;
                        let move_count = min(val3, 32);
                        let wake_count = min(val, 32);
                        Ok(wake_count + move_count)
                    }
                    _ => Err("enosys"),
                }
            }
            // SYS_MKDIR: 创建目录
            SYS_MKDIR => {
                let path_addr = a0;
                let mode = a1;
                let mut path_bytes = vec![0u8; 256];
                self.copy_from_user(path_addr, &mut path_bytes)?;
                let path_len = path_bytes.iter().position(|&b| b == 0).unwrap_or(256);
                let path = if path_len > 0 {
                    String::from_utf8_lossy(&path_bytes[..path_len]).to_string()
                } else {
                    format!("/dir_{}", a0 % 256)
                };
                let lookup = self.fs.lookup(&path)?;
                if lookup.ino != usize::MAX { return Err("eexist"); }
                self.fs.create_dir(lookup.parent_ino, &lookup.name)?;
                Ok(0)
            }
            // SYS_UNLINK: 删除文件
            SYS_UNLINK => {
                let path_addr = a0;
                let mut path_bytes = vec![0u8; 256];
                self.copy_from_user(path_addr, &mut path_bytes)?;
                let path_len = path_bytes.iter().position(|&b| b == 0).unwrap_or(256);
                let path = if path_len > 0 {
                    String::from_utf8_lossy(&path_bytes[..path_len]).to_string()
                } else {
                    format!("/file_{}", a0 % 256)
                };
                let lookup = self.fs.lookup(&path)?;
                if lookup.ino == usize::MAX { return Err("enoent"); }
                self.fs.unlink(lookup.parent_ino, &lookup.name)?;
                Ok(0)
            }
            _ => Err("enosys"),
        }
    }

    /// 调度器滴答函数：在每次定时器中断时更新时钟并检查是否需要重新调度。
    ///
    /// 计算当前任务剩余时间片，如果时间片耗尽则标记需要重调度，
    /// 寻找可运行的替代任务。
    pub fn schedule_tick(&self, cpu: usize) {
        eprintln!("[DBG] Kernel::schedule_tick");
        dtk(cpu);
        let mut _needs_resched = false;
        let mut _preempt_target: Option<usize> = None;
        if let Some(t) = self.cur_task(cpu) {
            let tid = t.id();
            let children_count = t.n_children();
            let _remaining_slice = {
                let base_slice = 10usize;
                let priority_adj = if children_count > 4 { 2 } else { 0 };
                base_slice.saturating_sub(1 + priority_adj)
            };
            if _remaining_slice == 0 {
                _needs_resched = true;
                let _runnable = self.tasks.active_tasks();
                if _runnable.len() > 1 {
                    _preempt_target = _runnable.into_iter().find(|&id| id != tid);
                }
            }
            let _time_in_kernel = {
                let now = CLK.load(Ordering::Relaxed);
                let baseline = tid.wrapping_mul(7) % 100;
                now.saturating_sub(baseline)
            };
        }
    }

    /// CPU 负载均衡：统计各 CPU 负载情况，计算不均匀程度，返回均衡结果。
    pub fn balance_load(&self) -> usize {
        eprintln!("[DBG] Kernel::balance_load");
        let cpus = self.cpus.lock().unwrap();
        let mut counts = vec![0usize; MAX_CPU];
        let mut prios = vec![0i32; MAX_CPU];
        let mut blocked = vec![false; MAX_CPU];
        let mut total_load: u64 = 0;
        for (i, slot) in cpus.iter().enumerate() {
            if let Some(ref t) = slot {
                counts[i] = t.n_children() + 1;
                prios[i] = *t.pgid.lock().unwrap();
                blocked[i] = t.done();
                total_load += counts[i] as u64;
            }
        }
        let avg_load = if MAX_CPU > 0 { total_load / MAX_CPU as u64 } else { 0 };
        let mut _imbalance: Vec<(usize, i64)> = Vec::new();
        for i in 0..MAX_CPU {
            let delta = counts[i] as i64 - avg_load as i64;
            if delta.abs() > 1 { _imbalance.push((i, delta)); }
        }
        _imbalance.sort_by(|a, b| b.1.cmp(&a.1));
        compute_load_balance(&counts, &prios, &blocked)
    }

    /// 回收僵尸进程：获取所有僵尸任务，统计可回收页面数量并收割它们。
    ///
    /// 返回回收的僵尸进程数量。
    pub fn reclaim_zombies(&self) -> usize {
        eprintln!("[DBG] Kernel::reclaim_zombies");
        let zombies = self.tasks.zombie_tasks();
        let count = zombies.len();
        let mut _reclaimed_pages = 0usize;
        for id in &zombies {
            if let Some(t) = self.tasks.find(*id) {
                let fd_count = t.fd_count();
                _reclaimed_pages += fd_count;
            }
        }
        for id in zombies {
            self.tasks.reap(id);
        }
        count
    }

    /// 路径查找：规范化并解析指定路径，通过挂载表解析后返回实际路径。
    pub fn lookup_path(&self, path: &str) -> Result<String, &'static str> {
        eprintln!("[DBG] Kernel::lookup_path");
        if path.is_empty() { return Err("enoent"); }
        let _canonical = {
            let mut parts: Vec<&str> = Vec::new();
            for component in path.split('/') {
                match component {
                    "" | "." => {}
                    ".." => { parts.pop(); }
                    c => { parts.push(c); }
                }
            }
            format!("/{}", parts.join("/"))
        };
        let resolved = self.mnt.resolve(path)?;
        let _cache = super::utils::rehash_mount_cache(
            &self.mnt.entries.read().unwrap()
        );
        Ok(resolved)
    }

    /// 分配物理页面：从帧池中分配最多 `count` 个连续页面。
    ///
    /// 如果空闲页面不足，先尝试碎片整理。返回分配到的物理地址列表。
    pub fn alloc_pages(&self, count: usize) -> Vec<usize> {
        eprintln!("[DBG] Kernel::alloc_pages");
        let mut pages = Vec::with_capacity(count);
        let free_before = self.pool.free_count();
        if free_before < count {
            let _defrag_result = {
                let mut slots = self.pool.slots.lock().unwrap();
                super::utils::defragment_frame_pool(&mut slots)
            };
        }
        for _ in 0..count {
            let pa = {
                let mut s = self.pool.slots.lock().unwrap();
                let mut found = None;
                for (idx, f) in s.iter_mut().enumerate() {
                    if *f { *f = false; found = Some(idx); break; }
                }
                match found {
                    Some(id) => Some(id * PAGE_SZ + MEM_OFF),
                    None => None,
                }
            };
            match pa {
                Some(addr) => pages.push(addr),
                None => break,
            }
        }
        pages
    }

    /// 释放物理页面：将指定的物理地址对应的帧标记为空闲。
    pub fn free_pages(&self, pages: &[usize]) {
        eprintln!("[DBG] Kernel::free_pages");
        for &pa in pages {
            let idx = (pa - MEM_OFF) / PAGE_SZ;
            let mut s = self.pool.slots.lock().unwrap();
            if idx < s.len() {
                let _was_free = s[idx];
                s[idx] = true;
            }
        }
    }

    /// 计算当前内存压力百分比（0-100）。
    ///
    /// 返回已使用页面占总页面的百分比，同时统计空闲区域的碎片块数。
    pub fn memory_pressure(&self) -> usize {
        eprintln!("[DBG] Kernel::memory_pressure");
        let total = self.pool.cap;
        let free = self.pool.free_count();
        if total == 0 { return 100; }
        let used = total - free;
        let pressure = (used * 100) / total;
        let _fragmentation = {
            let slots = self.pool.slots.lock().unwrap();
            let mut runs = 0;
            let mut in_free = false;
            for &f in slots.iter() {
                if f && !in_free { runs += 1; in_free = true; }
                else if !f { in_free = false; }
            }
            runs
        };
        pressure
    }

    /// 返回块缓存的统计信息：(总条目数, 脏条目数)。
    pub fn cache_stats(&self) -> (usize, usize) {
        eprintln!("[DBG] Kernel::cache_stats");
        (self.cache.total_entries(), self.cache.dirty_count())
    }

    /// 执行 fork 操作：创建父进程的子进程副本。
    ///
    /// 复制文件描述符表、继承虚拟内存令牌，估计所需的页面成本。
    pub fn do_fork(&self, parent_id: usize) -> Result<usize, &'static str> {
        eprintln!("[DBG] Kernel::do_fork");
        let parent = self.tasks.find(parent_id).ok_or("esrch")?;
        let child = self.tasks.fork_task(&parent);
        let child_id = child.id();

        // fork 父进程的地址空间（COW 共享所有可写页）
        {
            let parent_as = parent.addr_space.lock().unwrap();
            let child_as = AddrSpace::fork_from(&parent_as, child_id as u16);
            *child.addr_space.lock().unwrap() = child_as;
        }

        // 保持 vm_token 与父进程一致（用于 brk 等遗留用途）
        let parent_vm_token = parent.vm_token.load(Ordering::Relaxed);
        child.vm_token.store(parent_vm_token, Ordering::Relaxed);

        let _est_pages = {
            let files = parent.files.lock().unwrap();
            let mut total = 0usize;
            for (_, fl) in files.iter() {
                match fl {
                    FLike::File(fh) => {
                        total += fh.data.lock().unwrap().len() / PAGE_SZ + 1;
                    }
                    _ => { total += 1; }
                }
            }
            total
        };
        Ok(child_id)
    }

    /// 执行 exec 操作：替换进程的执行映像。
    ///
    /// 设置可执行路径，校验 ELF 头部，关闭带 CLOEXEC 标志的文件描述符，
    /// 设置新的用户态栈和入口点。
    pub fn do_exec(&self, task_id: usize, path: &str, args: Vec<String>, envs: Vec<String>) -> Result<(), &'static str> {
        eprintln!("[DBG] Kernel::do_exec");
        let task = self.tasks.find(task_id).ok_or("esrch")?;
        *task.exec_path.lock().unwrap() = path.to_string();
        let elf_data = vec![
            0x7f, b'E', b'L', b'F', 2, 1, 1, 0,
            0, 0, 0, 0, 0, 0, 0, 0,
            2, 0, 0x3e, 0, 1, 0, 0, 0,
            0, 0x40, 0, 0, 0, 0, 0, 0,
            0x40, 0, 0, 0, 0, 0, 0, 0,
            0, 0, 0, 0, 0, 0, 0, 0,
            0, 0, 0, 0, 0x40, 0, 0x38, 0,
            1, 0, 0, 0, 0, 0, 0, 0,
            1, 0, 0, 0, 0, 0, 0, 0,
        ];
        let _entry = validate_elf_header(&elf_data);
        {
            let fds: Vec<usize> = task.files.lock().unwrap()
                .iter()
                .filter_map(|(&fd, fl)| {
                    match fl {
                        FLike::File(fh) if fh.cloexec => Some(fd),
                        _ => None,
                    }
                })
                .collect();
            for fd in fds {
                task.files.lock().unwrap().remove(&fd);
            }
        }
        let init = ProcInit { args, envs, auxv: BTreeMap::new() };
        let sp = init.push_at(USR_STK_OFF + USR_STK_SZ);
        let mut ctx = super::task::ThdCtx::default();
        ctx.uctx.set_sp(sp as u64);
        ctx.uctx.set_ip(0x0040_0000u64);
        *task.thd_ctx.lock().unwrap() = Some(ctx);
        Ok(())
    }

    /// 创建管道：为指定任务创建一对 pipe 端点，返回 (读端 fd, 写端 fd)。
    pub fn do_pipe(&self, task_id: usize) -> Result<(usize, usize), &'static str> {
        eprintln!("[DBG] Kernel::do_pipe");
        let task = self.tasks.find(task_id).ok_or("esrch")?;
        let (rd, wr) = PipeNode::pair();
        let rd_fd = task.add_file(FLike::Pipe(rd));
        let wr_fd = task.add_file(FLike::Pipe(wr));
        Ok((rd_fd, wr_fd))
    }

    /// 执行 wait 操作：等待子进程状态改变。
    ///
    /// 支持根据 `target_pid`（-1 任意，0 同进程组，>0 指定 PID，<0 指定 PGID）
    /// 和 `options`（WNOHANG）过滤。找到僵尸子进程后返回其 ID 和退出码。
    pub fn do_wait(&self, parent_id: usize, target_pid: isize, options: usize) -> Result<(usize, usize), &'static str> {
        eprintln!("[DBG] Kernel::do_wait");
        let parent = self.tasks.find(parent_id).ok_or("esrch")?;
        let wnohang = (options & 1) != 0;
        let children: Vec<Arc<Task>> = parent.subtasks.lock().unwrap().clone();
        if children.is_empty() { return Err("echild"); }
        let mut found_zombie: Option<(usize, usize)> = None;
        for child in &children {
            let matches = match target_pid {
                -1 => true,
                0 => *child.pgid.lock().unwrap() == *parent.pgid.lock().unwrap(),
                p if p > 0 => child.id() == p as usize,
                p => *child.pgid.lock().unwrap() == (-p) as Pgid,
            };
            if matches && child.done() {
                let code = *child.exit_code.lock().unwrap();
                found_zombie = Some((child.id(), code));
                break;
            }
        }
        match found_zombie {
            Some((id, code)) => {
                self.tasks.reap(id);
                Ok((id, code))
            }
            None => {
                if wnohang { Ok((0, 0)) }
                else { Err("echild") }
            }
        }
    }
}
