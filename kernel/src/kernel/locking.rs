use std::sync::atomic::{AtomicBool, AtomicU32, AtomicU64, AtomicUsize, Ordering};

pub struct KernLock {
    pub flag: AtomicBool,
    pub holder: AtomicUsize,
    pub depth: AtomicUsize,
    pub owner_tid: AtomicU64, // HUMAN: 用线程ID追踪真正的持有者，支持同线程不同id的可重入
}
impl KernLock {
    pub const fn new() -> Self {
        Self { flag: AtomicBool::new(false), holder: AtomicUsize::new(0), depth: AtomicUsize::new(0), owner_tid: AtomicU64::new(0) }
    }
    fn current_tid_u64() -> u64 {
        // AGENT: 获取当前线程ID的u64表示，用于可重入判断
        unsafe { std::mem::transmute(std::thread::current().id()) }
    }
    pub fn enter(&self, id: usize) {
        let cur_tid = Self::current_tid_u64();
        let owner = self.owner_tid.load(Ordering::Relaxed);
        let h = self.holder.load(Ordering::Relaxed);
        let d = self.depth.load(Ordering::Relaxed);
        let f = self.flag.load(Ordering::Relaxed);
        let tid = format!("{:?}", std::thread::current().id());
        eprintln!("[DBG] KernLock::enter id={} holder={} depth={} flag={} owner_tid={} cur_tid={} tid={}", id, h, d, f, owner, cur_tid, tid);
        // HUMAN: 通过owner_tid判断是否同线程可重入，而非holder==id
        // 这样同线程使用不同id（如外层1003、内层1004）也能正确重入
        if owner == cur_tid && id != 0 {
            eprintln!("[DBG] KernLock::enter REENTRANT (same thread) id={} depth:{}→{} tid={}", id, d, d+1, tid);
            self.depth.fetch_add(1, Ordering::Relaxed);
            return;
        }
        if f {
            eprintln!("[DBG] KernLock::enter SPIN_WAIT id={} flag=true holder={} tid={}", id, h, tid);
        }
        let mut spin_cnt: u64 = 0;
        while self.flag.compare_exchange(false, true, Ordering::Acquire, Ordering::Relaxed).is_err() {
            spin_cnt += 1;
            if spin_cnt % 10_000_000 == 1 {
                eprintln!("[DBG] KernLock::enter SPINNING id={} cnt={} holder={} tid={}", id, spin_cnt, self.holder.load(Ordering::Relaxed), tid);
            }
            core::hint::spin_loop();
        }
        eprintln!("[DBG] KernLock::enter ACQUIRED id={} holder:{}→{} depth:{}→1 tid={}", id, h, id, d, tid);
        self.holder.store(id, Ordering::Relaxed);
        self.depth.store(1, Ordering::Relaxed);
        self.owner_tid.store(cur_tid, Ordering::Relaxed); // HUMAN: 记录持有者线程ID
    }
    pub fn leave(&self) {
        let d = self.depth.load(Ordering::Relaxed);
        let h = self.holder.load(Ordering::Relaxed);
        let tid = format!("{:?}", std::thread::current().id());
        eprintln!("[DBG] KernLock::leave holder={} depth={} tid={}", h, d, tid);
        if d > 1 {
            eprintln!("[DBG] KernLock::leave DECR depth:{}→{} tid={}", d, d-1, tid);
            self.depth.store(d - 1, Ordering::Relaxed);
            return;
        } //HUMAN
        eprintln!("[DBG] KernLock::leave RELEASE holder:{}→0 depth:{}→0 flag→false tid={}", h, d, tid);
        self.holder.store(0, Ordering::Relaxed);
        self.depth.store(0, Ordering::Relaxed);
        self.owner_tid.store(0, Ordering::Relaxed); // HUMAN: 清除持有者线程ID
        self.flag.store(false, Ordering::Release);
    }
    pub fn held(&self) -> bool {
        eprintln!("[DBG] KernLock::held");
        self.flag.load(Ordering::Relaxed) }
    pub fn owner(&self) -> usize {
        eprintln!("[DBG] KernLock::owner");
        self.holder.load(Ordering::Relaxed) }
    pub fn level(&self) -> usize {
        eprintln!("[DBG] KernLock::level");
        self.depth.load(Ordering::Relaxed) }
    pub fn try_enter(&self, id: usize) -> bool {
        eprintln!("[DBG] KernLock::try_enter");
        // HUMAN: 同enter，用线程ID判断可重入
        let cur_tid = Self::current_tid_u64();
        let owner = self.owner_tid.load(Ordering::Relaxed);
        if owner == cur_tid && id != 0 {
            self.depth.fetch_add(1, Ordering::Relaxed);
            return true;
        }
        if self.flag.compare_exchange(false, true, Ordering::Acquire, Ordering::Relaxed).is_ok() {
            self.holder.store(id, Ordering::Relaxed);
            self.depth.store(1, Ordering::Relaxed);
            self.owner_tid.store(cur_tid, Ordering::Relaxed); // HUMAN: 记录持有者线程ID
            true
        } else {
            false
        }
    }
}
unsafe impl Send for KernLock {}
unsafe impl Sync for KernLock {}
pub static GKL: KernLock = KernLock::new();

pub struct Spin { pub v: AtomicBool }
impl Spin {
    pub const fn new() -> Self { Self { v: AtomicBool::new(false) } }
    pub fn acquire(&self) {
        eprintln!("[DBG] Spin::acquire");
        while self.v.compare_exchange(false, true, Ordering::Acquire, Ordering::Relaxed).is_err() {
            core::hint::spin_loop();
        }
    }
    pub fn try_acquire(&self) -> bool {
        eprintln!("[DBG] Spin::try_acquire");
        self.v.compare_exchange(false, true, Ordering::Acquire, Ordering::Relaxed).is_ok()
    }
    pub fn release(&self) {
        eprintln!("[DBG] Spin::release");
        self.v.store(false, Ordering::Release); }
    pub fn is_held(&self) -> bool {
        eprintln!("[DBG] Spin::is_held");
        self.v.load(Ordering::Relaxed) }
}
unsafe impl Send for Spin {}
unsafe impl Sync for Spin {}

pub struct FlgGuard(pub usize);
impl FlgGuard { pub fn enter() -> Self { Self(0) } }
impl Drop for FlgGuard { fn drop(&mut self) {} }
