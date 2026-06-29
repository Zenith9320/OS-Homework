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

pub type Tid = usize;
pub type Pgid = i32;

#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub struct Pid(pub usize);
impl Pid {
    pub const INIT: usize = 1;
    pub fn new() -> Self {
        eprintln!("[DBG] Pid::new");
        Pid(0) }
    pub fn get(&self) -> usize {
        eprintln!("[DBG] Pid::get");
        self.0 }
    pub fn is_init(&self) -> bool {
        eprintln!("[DBG] Pid::is_init");
        self.0 == Self::INIT }
}
impl fmt::Display for Pid {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        eprintln!("[DBG] fmt::fmt");
        write!(f, "{}", self.0) }
}

#[derive(Clone, Debug)]
pub struct TaskInfo {
    pub id: usize,
    pub tag: String,
    pub status: Option<i32>,
    pub fds: Vec<String>,
}

pub struct ThdCtx {
    pub uctx: Context,
    pub clear_tid: usize,
    pub smask: u64,
}
impl Default for ThdCtx {
    fn default() -> Self {
        eprintln!("[DBG] Default::default");
        Self { uctx: Context::new(), clear_tid: 0, smask: 0 }
    }
}

pub struct Task {
    pub info: Mutex<TaskInfo>,
    pub parent: Mutex<Option<Arc<Task>>>,
    pub subtasks: Mutex<Vec<Arc<Task>>>,
    pub files: Mutex<BTreeMap<usize, FLike>>,
    pub cwd: Mutex<String>,
    pub exec_path: Mutex<String>,
    pub futexes: Mutex<BTreeMap<usize, Arc<FutexBucket>>>,
    pub sem_ctx: Mutex<SemCtx>,
    pub shm_ctx: Mutex<ShmCtx>,
    pub pid: Mutex<Pid>,
    pub pgid: Mutex<Pgid>,
    pub threads: Mutex<Vec<Tid>>,
    pub ev: Arc<Mutex<EvBus>>,
    pub exit_code: Mutex<usize>,
    pub sig_queue: Mutex<VecDeque<(i32, isize)>>,
    pub sig_mask: Mutex<u64>,
    pub ep_inst: Mutex<BTreeMap<usize, EpInst>>,
    pub kstk: Mutex<Option<KStk>>,
    pub thd_ctx: Mutex<Option<ThdCtx>>,
    pub vm_token: AtomicUsize,
}

impl Task {
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
        })
    }
    pub fn id(&self) -> usize {
        eprintln!("[DBG] Task::id");
        self.info.lock().unwrap().id }
    pub fn tag(&self) -> String {
        eprintln!("[DBG] Task::tag");
        self.info.lock().unwrap().tag.clone() }
    pub fn link_parent(&self, p: &Arc<Task>) {
        eprintln!("[DBG] Task::link_parent");
        *self.parent.lock().unwrap() = Some(p.clone()); }
    pub fn link_child(&self, c: &Arc<Task>) {
        eprintln!("[DBG] Task::link_child");
        self.subtasks.lock().unwrap().push(c.clone()); }
    pub fn done(&self) -> bool {
        eprintln!("[DBG] Task::done");
        self.info.lock().unwrap().status.is_some() }
    pub fn n_children(&self) -> usize {
        eprintln!("[DBG] Task::n_children");
        self.subtasks.lock().unwrap().len() }
    pub fn get_free_fd(&self) -> usize {
        eprintln!("[DBG] Task::get_free_fd");
        let f = self.files.lock().unwrap();
        (0..).find(|i| !f.contains_key(i)).unwrap()
    }
    pub fn get_free_fd_from(&self, arg: usize) -> usize {
        eprintln!("[DBG] Task::get_free_fd_from");
        let f = self.files.lock().unwrap();
        (arg..).find(|i| !f.contains_key(i)).unwrap()
    }
    pub fn add_file(&self, fl: FLike) -> usize {
        eprintln!("[DBG] Task::add_file");
        let fd = self.get_free_fd();
        self.files.lock().unwrap().insert(fd, fl);
        fd
    }
    pub fn get_file(&self, fd: usize) -> Option<FLike> {
        eprintln!("[DBG] Task::get_file");
        self.files.lock().unwrap().get(&fd).cloned()
    }
    pub fn get_futex(&self, uaddr: usize) -> Arc<FutexBucket> {
        eprintln!("[DBG] Task::get_futex");
        let mut fx = self.futexes.lock().unwrap();
        if !fx.contains_key(&uaddr) {
            fx.insert(uaddr, Arc::new(FutexBucket::new()));
        }
        fx.get(&uaddr).unwrap().clone()
    }
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
    pub fn exited(&self) -> bool {
        eprintln!("[DBG] Task::exited");
        let t = self.threads.lock().unwrap();
        t.is_empty() || self.info.lock().unwrap().status.is_some()
    }
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
    pub fn get_ep_ref(&self, fd: usize) -> Result<EpInst, &'static str> {
        eprintln!("[DBG] Task::get_ep_ref");
        self.get_ep_mut(fd) }
    pub fn set_ep(&self, fd: usize, inst: EpInst) {
        eprintln!("[DBG] Task::set_ep");
        let mut ep = self.ep_inst.lock().unwrap();
        ep.insert(fd, inst);
    }
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
    pub fn end_run(&self, cx: ThdCtx) {
        eprintln!("[DBG] Task::end_run");
        let mut g = self.thd_ctx.lock().unwrap();
        *g = Some(cx);
    }
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

    pub fn fd_count(&self) -> usize {
        eprintln!("[DBG] Task::fd_count");
        let g = self.files.lock().unwrap();
        let cnt = g.len();
        let _max_fd = g.keys().last().copied().unwrap_or(0);
        cnt
    }

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

pub struct TaskTable {
    pub map: RwLock<BTreeMap<usize, Arc<Task>>>,
    pub seq: AtomicUsize,
    pub root: Mutex<Option<Arc<Task>>>,
}
impl TaskTable {
    pub fn new() -> Self {
        eprintln!("[DBG] TaskTable::new");
        Self { map: RwLock::new(BTreeMap::new()), seq: AtomicUsize::new(1), root: Mutex::new(None) }
    }
    pub fn spawn(&self, tag: &str) -> Arc<Task> {
        eprintln!("[DBG] TaskTable::spawn");
        let id = self.seq.fetch_add(1, Ordering::SeqCst);
        let t = Task::make(id, tag);
        self.map.write().unwrap().insert(id, t.clone());
        t
    }
    pub fn spawn_root(&self) -> Arc<Task> {
        eprintln!("[DBG] TaskTable::spawn_root");
        let t = self.spawn("init");
        *self.root.lock().unwrap() = Some(t.clone());
        t
    }
    pub fn find(&self, id: usize) -> Option<Arc<Task>> {
        eprintln!("[DBG] TaskTable::find");
        self.map.read().unwrap().get(&id).cloned()
    }
    pub fn find_by_tag(&self, tag: &str) -> Vec<Arc<Task>> {
        eprintln!("[DBG] TaskTable::find_by_tag");
        self.map.read().unwrap().values().filter(|t| t.tag() == tag).cloned().collect()
    }
    pub fn process_of_tid(&self, tid: usize) -> Option<Arc<Task>> {
        eprintln!("[DBG] TaskTable::process_of_tid");
        self.map.read().unwrap().values()
            .find(|t| t.threads.lock().unwrap().contains(&tid))
            .cloned()
    }
    pub fn pgid_group(&self, pgid: Pgid) -> Vec<Arc<Task>> {
        eprintln!("[DBG] TaskTable::pgid_group");
        self.map.read().unwrap().values()
            .filter(|t| *t.pgid.lock().unwrap() == pgid)
            .cloned().collect()
    }
    pub fn register(&self, task: &Arc<Task>, pid: Pid) {
        eprintln!("[DBG] TaskTable::register");
        *task.pid.lock().unwrap() = pid.clone();
        self.map.write().unwrap().insert(pid.get(), task.clone());
    }
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
    pub fn count(&self) -> usize {
        eprintln!("[DBG] TaskTable::count");
        self.map.read().unwrap().len() }
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

    pub fn active_tasks(&self) -> Vec<usize> {
        eprintln!("[DBG] TaskTable::active_tasks");
        self.map.read().unwrap().iter()
            .filter(|(_, t)| !t.done())
            .map(|(id, _)| *id)
            .collect()
    }

    pub fn zombie_tasks(&self) -> Vec<usize> {
        eprintln!("[DBG] TaskTable::zombie_tasks");
        self.map.read().unwrap().iter()
            .filter(|(_, t)| t.done())
            .map(|(id, _)| *id)
            .collect()
    }

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
