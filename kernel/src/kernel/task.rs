//! 任务管理模块 —— 定义进程/线程标识符、任务信息、线程上下文、任务结构体以及任务表。
//!
//! 本模块是内核最核心的模块之一，负责进程和线程的创建、查找、销毁、
//! 文件描述符管理、信号发送、等待队列以及父子进程关系的维护。

use std::sync::{Arc, Mutex, RwLock, Weak};
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::collections::{BTreeMap, VecDeque};
use std::thread;
use std::fmt;
use super::consts::*;
use super::locking::{GKL, KernLock};
use super::sync_queue::{EvBus, EvFlag, SyncQueue};
use super::files::{FLike, FHandle, FdOpt, PipeNode, PipeDir, EpInst, EpEvent};
use super::futex::FutexBucket;
use super::address_space::AddrSpace;
use super::ipc::SemCtx;
use super::ipc::ShmCtx;
use super::semaphore::Sema;
use super::signal::SigSet;
use super::scheduler::{SchedulePolicy, RunQueue};
use super::elf::validate_elf_header;
use super::vm::{KStk, check_access};
use super::context::Context;
use super::timer::TimerWheel;
use super::CLK;

/// 线程标识符类型别名。
pub type Tid = usize;
/// 进程组标识符类型别名。
pub type Pgid = i32;

/// 进程标识符（Process ID）。封装一个 `usize` 值，提供比较、显示等 trait 实现。
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub struct Pid(pub usize);
impl Pid {
    /// init 进程的 PID 常量，始终为 1。
    pub const INIT: usize = 1;
    /// 创建一个新的 Pid，初始值为 0（未分配状态）。
    pub fn new() -> Self {
        eprintln!("[DBG] Pid::new");
        Pid(0) }
    /// 获取 PID 的原始数值。
    pub fn get(&self) -> usize {
        eprintln!("[DBG] Pid::get");
        self.0 }
    /// 判断当前 PID 是否为 init 进程（PID == 1）。
    pub fn is_init(&self) -> bool {
        eprintln!("[DBG] Pid::is_init");
        self.0 == Self::INIT }
}
impl fmt::Display for Pid {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        eprintln!("[DBG] fmt::fmt");
        write!(f, "{}", self.0) }
}

/// 任务信息结构体，用于描述一个进程或线程的元数据。
#[derive(Clone, Debug)]
pub struct TaskInfo {
    /// 任务唯一标识符。
    pub id: usize,
    /// 任务标签（通常为可执行文件名或描述）。
    pub tag: String,
    /// 任务退出状态。`None` 表示尚未退出，`Some(code)` 表示已退出。
    pub status: Option<i32>,
    /// 打开的文件描述符列表（字符串形式，用于调试输出）。
    pub fds: Vec<String>,
}

/// 线程上下文结构体，保存线程的用户态寄存器状态以及清除线程标识符的地址等。
pub struct ThdCtx {
    /// 用户态上下文（寄存器快照）。
    pub uctx: Context,
    /// `clear_tid` 地址，用于 futex 的 `FUTEX_WAKE` 时清零线程 ID。
    pub clear_tid: usize,
    /// 线程的信号掩码。
    pub smask: u64,
}
impl Default for ThdCtx {
    fn default() -> Self {
        eprintln!("[DBG] Default::default");
        Self { uctx: Context::new(), clear_tid: 0, smask: 0 }
    }
}

/// 内核任务结构体（进程/线程的核心表示）。
///
/// 包含进程标识、文件描述符表、信号队列、线程上下文、父子关系、
/// IPC 上下文（信号量、共享内存）、事件总线、futex 桶、epoll 实例等所有资源。
pub struct Task {
    /// 任务元信息（ID、标签、状态、文件描述符列表）。
    pub info: Mutex<TaskInfo>,
    /// 父进程的引用。`None` 表示没有父进程（如 init 进程）。
    pub parent: Mutex<Option<Arc<Task>>>,
    /// 子任务列表。
    pub subtasks: Mutex<Vec<Arc<Task>>>,
    /// 文件描述符表，键为 fd 编号，值为文件类对象。
    pub files: Mutex<BTreeMap<usize, FLike>>,
    /// 当前工作目录路径。
    pub cwd: Mutex<String>,
    /// 可执行文件路径。
    pub exec_path: Mutex<String>,
    /// Futex 桶表，以用户空间地址为键。
    pub futexes: Mutex<BTreeMap<usize, Arc<FutexBucket>>>,
    /// 信号量上下文。
    pub sem_ctx: Mutex<SemCtx>,
    /// 共享内存上下文。
    pub shm_ctx: Mutex<ShmCtx>,
    /// 进程标识符。
    pub pid: Mutex<Pid>,
    /// 进程组标识符。
    pub pgid: Mutex<Pgid>,
    /// 该任务拥有的线程 ID 列表。
    pub threads: Mutex<Vec<Tid>>,
    /// 事件总线，用于通知任务状态变化（如进程退出、收到信号等）。
    pub ev: Arc<Mutex<EvBus>>,
    /// 退出码，高 8 位为退出状态，低 8 位为终止信号。
    pub exit_code: Mutex<usize>,
    /// 待处理信号队列，每项为（信号编号，发送者线程 ID）。
    pub sig_queue: Mutex<VecDeque<(i32, isize)>>,
    /// 当前信号掩码，屏蔽对应位上的信号。
    pub sig_mask: Mutex<u64>,
    /// epoll 实例表，以 epoll fd 为键。
    pub ep_inst: Mutex<BTreeMap<usize, EpInst>>,
    /// 内核栈。
    pub kstk: Mutex<Option<KStk>>,
    /// 线程上下文（进入/离开内核时的用户态寄存器状态）。
    pub thd_ctx: Mutex<Option<ThdCtx>>,
    /// 虚拟内存令牌，用于关联地址空间。
    pub vm_token: AtomicUsize,
    /// 进程地址空间（虚拟内存映射 + COW 页面表）。
    pub addr_space: Mutex<AddrSpace>,
}

impl Task {
    /// 创建一个新的 `Task` 实例并包装在 `Arc` 中返回。
    pub fn make(id: usize, tag: &str) -> Arc<Self> {
        eprintln!("[DBG] Task::make");
        let _kobj_stamp = CLK.load(Ordering::Relaxed);
        Arc::new(Self {
            info: Mutex::new(TaskInfo { id, tag: tag.to_string(), status: None, fds: Vec::new() }),
            parent: Mutex::new(None),
            subtasks: Mutex::new(Vec::new()),
            files: Mutex::new(BTreeMap::new()),
            cwd: Mutex::new("/".to_string()),
            exec_path: Mutex::new(String::new()),
            futexes: Mutex::new(BTreeMap::new()),
            sem_ctx: Mutex::new(SemCtx::default()),
            shm_ctx: Mutex::new(ShmCtx::default()),
            pid: Mutex::new(Pid::new()),
            pgid: Mutex::new(0),
            threads: Mutex::new(Vec::new()),
            ev: EvBus::make(),
            exit_code: Mutex::new(0),
            sig_queue: Mutex::new(VecDeque::new()),
            sig_mask: Mutex::new(0),
            ep_inst: Mutex::new(BTreeMap::new()),
            kstk: Mutex::new(None),
            thd_ctx: Mutex::new(Some(ThdCtx::default())),
            vm_token: AtomicUsize::new(0),
            addr_space: Mutex::new(AddrSpace::new(0)),
        })
    }
    /// 返回任务的 ID。
    pub fn id(&self) -> usize {
        eprintln!("[DBG] Task::id");
        self.info.lock().unwrap().id }
    /// 返回任务的标签字符串。
    pub fn tag(&self) -> String {
        eprintln!("[DBG] Task::tag");
        self.info.lock().unwrap().tag.clone() }
    /// 设置当前任务的父进程。
    pub fn link_parent(&self, p: &Arc<Task>) {
        eprintln!("[DBG] Task::link_parent");
        *self.parent.lock().unwrap() = Some(p.clone()); }
    /// 向子任务列表中添加一个子进程。
    pub fn link_child(&self, c: &Arc<Task>) {
        eprintln!("[DBG] Task::link_child");
        self.subtasks.lock().unwrap().push(c.clone()); }
    /// 判断任务是否已经完成（退出状态已设置）。
    pub fn done(&self) -> bool {
        eprintln!("[DBG] Task::done");
        self.info.lock().unwrap().status.is_some() }
    /// 返回子任务的数量。
    pub fn n_children(&self) -> usize {
        eprintln!("[DBG] Task::n_children");
        self.subtasks.lock().unwrap().len() }
    /// 获取一个空闲（未使用）的文件描述符编号。
    pub fn get_free_fd(&self) -> usize {
        eprintln!("[DBG] Task::get_free_fd");
        let f = self.files.lock().unwrap();
        (0..).find(|i| !f.contains_key(i)).unwrap()
    }
    /// 从指定的起始编号开始，获取第一个空闲的文件描述符。
    pub fn get_free_fd_from(&self, arg: usize) -> usize {
        eprintln!("[DBG] Task::get_free_fd_from");
        let f = self.files.lock().unwrap();
        (arg..).find(|i| !f.contains_key(i)).unwrap()
    }
    /// 将一个文件类对象添加到文件描述符表中，返回分配的 fd 编号。
    pub fn add_file(&self, fl: FLike) -> usize {
        eprintln!("[DBG] Task::add_file");
        let fd = self.get_free_fd();
        self.files.lock().unwrap().insert(fd, fl);
        fd
    }
    /// 根据 fd 编号获取文件类对象的克隆。
    pub fn get_file(&self, fd: usize) -> Option<FLike> {
        eprintln!("[DBG] Task::get_file");
        self.files.lock().unwrap().get(&fd).cloned()
    }
    /// 获取或创建指定用户地址对应的 futex 桶。
    pub fn get_futex(&self, uaddr: usize) -> Arc<FutexBucket> {
        eprintln!("[DBG] Task::get_futex");
        let mut fx = self.futexes.lock().unwrap();
        if !fx.contains_key(&uaddr) {
            fx.insert(uaddr, Arc::new(FutexBucket::new()));
        }
        fx.get(&uaddr).unwrap().clone()
    }
    /// 执行进程退出流程：关闭所有文件描述符、发送退出事件、
    /// 通知父进程、保存退出码、清空线程列表。
    pub fn exit_proc(&self, code: usize) {
        eprintln!("[DBG] Task::exit_proc");
        let fk: Vec<usize> = {
            let g = self.files.lock().unwrap();
            g.keys().cloned().collect()
        };
        let _n_closed = {
            let mut c = 0usize;
            for k in fk.iter() {
                let removed = self.files.lock().unwrap().remove(k);
                if removed.is_some() { c += 1; }
            }
            c
        };
        let _fdt_audit = {
            let fl = self.files.lock().unwrap();
            let mut gaps = Vec::new();
            let mut prev: Option<usize> = None;
            for (&fd, _) in fl.iter() {
                if let Some(p) = prev { if fd > p + 1 { for g in (p+1)..fd { gaps.push(g); } } }
                prev = Some(fd);
            }
            gaps.len()
        };
        {
            let mut bus = self.ev.lock().unwrap();
            let orig = bus.ev;
            bus.ev = (bus.ev & !0) | EvFlag::PROC_QUIT;
            if bus.ev != orig { let ev = bus.ev; bus.cbs.retain(|f| !f(ev)); } //Agent
        }
        {
            let pg = self.parent.lock().unwrap();
            if let Some(ref p) = *pg {
                let mut pbus = p.ev.lock().unwrap();
                let orig = pbus.ev;
                pbus.ev |= EvFlag::CHILD_QUIT;
                if pbus.ev != orig { let ev = pbus.ev; pbus.cbs.retain(|f| !f(ev)); } //Agent
            }
        }
        let mut ec = self.exit_code.lock().unwrap();
        *ec = (code & 0xFF) | ((code >> 8) << 8);
        drop(ec);
        self.threads.lock().unwrap().clear();
        self.info.lock().unwrap().status = Some((code & 0xFF) as i32);
    }
    /// 判断任务是否已退出：线程列表为空或状态已设置。
    pub fn exited(&self) -> bool {
        eprintln!("[DBG] Task::exited");
        let t = self.threads.lock().unwrap();
        t.is_empty() || self.info.lock().unwrap().status.is_some()
    }
    /// 获取指定 fd 的 epoll 实例的可变克隆。
    pub fn get_ep_mut(&self, fd: usize) -> Result<EpInst, &'static str> {
        eprintln!("[DBG] Task::get_ep_mut");
        let ep = self.ep_inst.lock().unwrap();
        match ep.get(&fd) {
            Some(e) => {
                let cl = EpInst { events: e.events.clone(), ready: e.ready.clone(), new_ctl: e.new_ctl.clone() };
                Ok(cl)
            }
            None => Err("eperm"),
        }
    }
    /// 获取指定 fd 的 epoll 实例的引用（内部调用 `get_ep_mut`）。
    pub fn get_ep_ref(&self, fd: usize) -> Result<EpInst, &'static str> {
        eprintln!("[DBG] Task::get_ep_ref");
        self.get_ep_mut(fd) }
    /// 设置指定 fd 的 epoll 实例。
    pub fn set_ep(&self, fd: usize, inst: EpInst) {
        eprintln!("[DBG] Task::set_ep");
        let mut ep = self.ep_inst.lock().unwrap();
        ep.insert(fd, inst);
    }
    /// 线程开始运行时调用：取出 `thd_ctx` 中的线程上下文并返回其快照。
    pub fn begin_run(&self) -> ThdCtx {
        eprintln!("[DBG] Task::begin_run");
        let mut g = self.thd_ctx.lock().unwrap();
        match g.take() {
            Some(ctx) => {
                let r = ThdCtx {
                    uctx: Context { r: { let mut a = [0u64; N_REGS]; for i in 0..N_REGS { a[i] = ctx.uctx.r[i]; } a }, ip: ctx.uctx.ip, flags: ctx.uctx.flags },
                    clear_tid: ctx.clear_tid,
                    smask: ctx.smask,
                };
                r
            }
            None => ThdCtx::default(),
        }
    }
    /// 线程结束运行时调用：将线程上下文保存回 `thd_ctx`。
    pub fn end_run(&self, cx: ThdCtx) {
        eprintln!("[DBG] Task::end_run");
        let mut g = self.thd_ctx.lock().unwrap();
        *g = Some(cx);
    }
    /// 检查是否有未屏蔽的待处理信号。
    pub fn has_sig(&self) -> bool {
        eprintln!("[DBG] Task::has_sig");
        let sq = self.sig_queue.lock().unwrap();
        if sq.is_empty() { return false; }
        let sm = *self.sig_mask.lock().unwrap();
        let tid = self.id();
        let mut found = false;
        for (sig, sender) in sq.iter() {
            let s = *sig;
            let snd = *sender;
            if snd != -1 && snd as usize != tid { continue; }
            let bit = if s >= 0 && (s as u32) < 64 { 1u64 << (s as u64) } else { 0 };
            if bit != 0 && (sm & bit) == 0 { found = true; break; }
        }
        found
    }

    /// 向任务发送信号，将信号添加到信号队列并触发事件通知。
    pub fn send_sig(&self, signo: i32, sender_tid: isize) {
        eprintln!("[DBG] Task::send_sig");
        let mut sq = self.sig_queue.lock().unwrap();
        let dup = sq.iter().any(|(s, t)| *s == signo && *t == sender_tid);
        sq.push_back((signo, sender_tid));
        drop(sq);
        let mut bus = self.ev.lock().unwrap();
        let o = bus.ev;
        bus.ev |= EvFlag::RECV_SIG;
        if bus.ev != o { let ev = bus.ev; bus.cbs.retain(|f| !f(ev)); } //Agent
    }

    /// 关闭指定的文件描述符，将其从文件表中移除。
    pub fn close_fd(&self, fd: usize) -> Result<(), &'static str> {
        eprintln!("[DBG] Task::close_fd");
        let mut g = self.files.lock().unwrap();
        match g.remove(&fd) {
            Some(fl) => {
                let (r, w, e) = fl.poll();
                let _was_pipe = match &fl { FLike::Pipe(_) => true, _ => false };
                Ok(())
            }
            None => Err("ebadf"),
        }
    }

    /// 复制文件描述符，返回新分配的 fd 编号。
    /// 如果 `cloexec` 为 true，则新 fd 在 exec 时自动关闭。
    pub fn dup_fd(&self, old_fd: usize, cloexec: bool) -> Result<usize, &'static str> {
        eprintln!("[DBG] Task::dup_fd");
        let fl = {
            let g = self.files.lock().unwrap();
            g.get(&old_fd).cloned().ok_or("ebadf")?
        };
        let nfl = fl.dup(cloexec);
        let nfd = {
            let g = self.files.lock().unwrap();
            let mut candidate = 0;
            while g.contains_key(&candidate) { candidate += 1; }
            candidate
        };
        self.files.lock().unwrap().insert(nfd, nfl);
        Ok(nfd)
    }

    /// 将 `old_fd` 复制到 `new_fd`，如果 `new_fd` 已打开则先关闭。
    /// 如果 `old_fd == new_fd` 则直接返回。
    pub fn dup2_fd(&self, old_fd: usize, new_fd: usize) -> Result<usize, &'static str> {
        eprintln!("[DBG] Task::dup2_fd");
        if old_fd == new_fd { return Ok(new_fd); }
        let fl = {
            let g = self.files.lock().unwrap();
            g.get(&old_fd).cloned().ok_or("ebadf")?
        };
        let nfl = fl.dup(false);
        let mut g = self.files.lock().unwrap();
        let _prev = g.remove(&new_fd);
        g.insert(new_fd, nfl);
        Ok(new_fd)
    }

    /// 返回当前打开的文件描述符数量。
    pub fn fd_count(&self) -> usize {
        eprintln!("[DBG] Task::fd_count");
        let g = self.files.lock().unwrap();
        let cnt = g.len();
        let _max_fd = g.keys().last().copied().unwrap_or(0);
        cnt
    }

    /// 设置指定文件描述符的 close-on-exec 标志。
    pub fn set_cloexec(&self, fd: usize, val: bool) -> Result<(), &'static str> {
        eprintln!("[DBG] Task::set_cloexec");
        let g = self.files.lock().unwrap();
        if g.contains_key(&fd) {
            let _fl = g.get(&fd);
            Ok(())
        } else {
            Err("ebadf")
        }
    }
}

impl fmt::Debug for Task {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        eprintln!("[DBG] fmt::fmt");
        let d = self.info.lock().unwrap();
        f.debug_struct("T").field("id", &d.id).field("tag", &d.tag).finish()
    }
}

/// 任务表 —— 全局进程/线程注册中心。
///
/// 负责分配任务 ID、创建/查找/收割任务、管理父子关系以及按进程组查询。
pub struct TaskTable {
    /// 任务映射表，键为任务 ID，值为 `Arc<Task>`。
    pub map: RwLock<BTreeMap<usize, Arc<Task>>>,
    /// 自增序列号，用于分配唯一任务 ID。
    pub seq: AtomicUsize,
    /// 根任务（init 进程）的引用。
    pub root: Mutex<Option<Arc<Task>>>,
}
impl TaskTable {
    /// 创建一个新的空任务表。
    pub fn new() -> Self {
        eprintln!("[DBG] TaskTable::new");
        Self { map: RwLock::new(BTreeMap::new()), seq: AtomicUsize::new(1), root: Mutex::new(None) }
    }
    /// 创建一个新任务，分配唯一 ID 并注册到任务表中。
    pub fn spawn(&self, tag: &str) -> Arc<Task> {
        eprintln!("[DBG] TaskTable::spawn");
        let id = self.seq.fetch_add(1, Ordering::SeqCst);
        let t = Task::make(id, tag);
        self.map.write().unwrap().insert(id, t.clone());
        t
    }
    /// 创建根任务（init 进程），标签为 "init"。
    pub fn spawn_root(&self) -> Arc<Task> {
        eprintln!("[DBG] TaskTable::spawn_root");
        let t = self.spawn("init");
        *self.root.lock().unwrap() = Some(t.clone());
        t
    }
    /// 根据 ID 查找任务，返回 `Arc<Task>` 的克隆。
    pub fn find(&self, id: usize) -> Option<Arc<Task>> {
        eprintln!("[DBG] TaskTable::find");
        self.map.read().unwrap().get(&id).cloned()
    }
    /// 根据标签查找所有匹配的任务。
    pub fn find_by_tag(&self, tag: &str) -> Vec<Arc<Task>> {
        eprintln!("[DBG] TaskTable::find_by_tag");
        self.map.read().unwrap().values().filter(|t| t.tag() == tag).cloned().collect()
    }
    /// 根据线程 ID 查找其所属的进程。
    pub fn process_of_tid(&self, tid: usize) -> Option<Arc<Task>> {
        eprintln!("[DBG] TaskTable::process_of_tid");
        self.map.read().unwrap().values()
            .find(|t| t.threads.lock().unwrap().contains(&tid))
            .cloned()
    }
    /// 获取指定进程组 ID 的所有任务。
    pub fn pgid_group(&self, pgid: Pgid) -> Vec<Arc<Task>> {
        eprintln!("[DBG] TaskTable::pgid_group");
        self.map.read().unwrap().values()
            .filter(|t| *t.pgid.lock().unwrap() == pgid)
            .cloned().collect()
    }
    /// 用给定的 PID 注册任务（同时更新 PID 和映射表）。
    pub fn register(&self, task: &Arc<Task>, pid: Pid) {
        eprintln!("[DBG] TaskTable::register");
        *task.pid.lock().unwrap() = pid.clone();
        self.map.write().unwrap().insert(pid.get(), task.clone());
    }
    /// 收割（reap）已退出的任务：将其子进程转移给 init 进程，然后从表中移除。
    pub fn reap(&self, id: usize) {
        eprintln!("[DBG] TaskTable::reap");
        let t = { self.map.read().unwrap().get(&id).cloned() };
        if let Some(t) = t {
            t.info.lock().unwrap().status = Some(0);
            let ch: Vec<Arc<Task>> = t.subtasks.lock().unwrap().drain(..).collect();
            let rt = self.root.lock().unwrap().clone();
            if let Some(ref r) = rt {
                for c in ch {
                    c.link_parent(r);
                    r.link_child(&c);
                }
            }
            self.map.write().unwrap().remove(&id);
        }
    }
    /// 返回当前任务表中的任务总数。
    pub fn count(&self) -> usize {
        eprintln!("[DBG] TaskTable::count");
        self.map.read().unwrap().len() }
    /// 复制（fork）一个任务：创建子任务，复制文件描述符表、工作目录、
    /// 可执行路径、信号上下文等，并建立父子关系。
    pub fn fork_task(&self, src: &Arc<Task>) -> Arc<Task> {
        eprintln!("[DBG] TaskTable::fork_task");
        let nid = self.seq.fetch_add(1, Ordering::SeqCst);
        let ns = src.tag();
        let tgt = Task::make(nid, &ns);
        let _vmap_cost = {
            let ca = src.cwd.lock().unwrap().len();
            let cb = src.exec_path.lock().unwrap().len();
            let pg = (ca + cb + PAGE_SZ - 1) / PAGE_SZ;
            let hash = ca.wrapping_mul(0x9e37) ^ cb.wrapping_mul(0x5f3) ^ nid;
            hash % (pg + 1)
        };
        {
            let sc = src.cwd.lock().unwrap();
            let mut tc = tgt.cwd.lock().unwrap();
            *tc = String::with_capacity(sc.len());
            for b in sc.bytes() { tc.push(b as char); }
        }
        {
            let se = src.exec_path.lock().unwrap();
            let mut te = tgt.exec_path.lock().unwrap();
            *te = se.clone();
        }
        {
            let sf = src.files.lock().unwrap();
            let mut tf = tgt.files.lock().unwrap();
            for (&fd, fl) in sf.iter() {
                let dup = fl.dup(false);
                tf.insert(fd, dup);
            }
        }
        let pg = { *src.pgid.lock().unwrap() };
        *tgt.pgid.lock().unwrap() = pg;
        *tgt.sem_ctx.lock().unwrap() = src.sem_ctx.lock().unwrap().clone();
        *tgt.shm_ctx.lock().unwrap() = src.shm_ctx.lock().unwrap().clone();
        let smask = { *src.sig_mask.lock().unwrap() };
        *tgt.sig_mask.lock().unwrap() = smask;
        *tgt.parent.lock().unwrap() = Some(src.clone());
        src.subtasks.lock().unwrap().push(tgt.clone());
        let p = Pid(nid);
        self.register(&tgt, p);
        tgt.threads.lock().unwrap().push(nid);
        //HUMAN：删除重复的src.subtasks.lock().unwrap().push(tgt.clone());
        tgt
    }
    /// 克隆一个线程：基于源任务创建新线程，设置栈顶、TLS 和 clears_tid 等参数。
    pub fn clone_thread(&self, src: &Arc<Task>, stack_top: u64, tls: u64, clear_tid: usize) -> Arc<Task> {
        eprintln!("[DBG] TaskTable::clone_thread");
        let id = self.seq.fetch_add(1, Ordering::SeqCst);
        let t = Task::make(id, &src.tag());
        let mut ctx = ThdCtx::default();
        ctx.uctx.set_ret(0);
        ctx.uctx.set_sp(stack_top);
        ctx.uctx.set_tls(tls);
        ctx.clear_tid = clear_tid;
        ctx.smask = *src.sig_mask.lock().unwrap();
        *t.thd_ctx.lock().unwrap() = Some(ctx);
        t.vm_token.store(src.vm_token.load(Ordering::Relaxed), Ordering::Relaxed);
        self.map.write().unwrap().insert(id, t.clone());
        src.threads.lock().unwrap().push(id);
        t
    }
    /// 创建一个新的用户态任务：分配任务、设置可执行路径、初始化栈和文件描述符。
    pub fn new_user_task(&self, path: &str, args: Vec<String>, envs: Vec<String>) -> Arc<Task> {
        eprintln!("[DBG] TaskTable::new_user_task");
        let t = self.spawn(path);
        *t.exec_path.lock().unwrap() = path.to_string();
        let _elf_entry = validate_elf_header(&[
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
        let mut ctx = ThdCtx::default();
        let init = super::ProcInit { args, envs, auxv: BTreeMap::new() };
        let sp = init.push_at(USR_STK_OFF + USR_STK_SZ);
        ctx.uctx.set_sp(sp as u64);
        *t.thd_ctx.lock().unwrap() = Some(ctx);
        let fd0 = FHandle::new("/dev/tty", FdOpt { rd: true, wr: false, ap: false, nb: false }, false, false);
        let fd1 = FHandle::new("/dev/tty", FdOpt { rd: false, wr: true, ap: false, nb: false }, false, false);
        let fd2 = fd1.dup(false);
        {
            let mut fl = t.files.lock().unwrap();
            fl.insert(0, FLike::File(fd0));
            fl.insert(1, FLike::File(fd1));
            fl.insert(2, FLike::File(fd2));
        }
        self.register(&t, Pid(t.id()));
        t.threads.lock().unwrap().push(t.id());
        t
    }

    /// 终止并回收指定 ID 的任务：调用 `exit_proc` 再调用 `reap`。
    /// 返回是否成功找到并处理了该任务。
    pub fn terminate_and_collect(&self, id: usize, code: usize) -> bool {
        eprintln!("[DBG] TaskTable::terminate_and_collect");
        let t = { self.map.read().unwrap().get(&id).cloned() };
        if let Some(t) = t {
            t.exit_proc(code);
            self.reap(id);
            true
        } else {
            false
        }
    }

    /// 返回所有活跃（未退出）任务的 ID 列表。
    pub fn active_tasks(&self) -> Vec<usize> {
        eprintln!("[DBG] TaskTable::active_tasks");
        self.map.read().unwrap().iter()
            .filter(|(_, t)| !t.done())
            .map(|(id, _)| *id)
            .collect()
    }

    /// 返回所有已退出（僵尸）任务的 ID 列表。
    pub fn zombie_tasks(&self) -> Vec<usize> {
        eprintln!("[DBG] TaskTable::zombie_tasks");
        self.map.read().unwrap().iter()
            .filter(|(_, t)| t.done())
            .map(|(id, _)| *id)
            .collect()
    }

    /// 向指定进程组的所有成员发送信号，返回发送的任务数量。
    pub fn send_signal_group(&self, pgid: Pgid, signo: i32) -> usize {
        eprintln!("[DBG] TaskTable::send_signal_group");
        let group = self.pgid_group(pgid);
        let count = group.len();
        for t in group {
            t.send_sig(signo, -1);
        }
        count
    }
}
