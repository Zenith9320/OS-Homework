//! 内核同步原语：自旋锁、内核全局锁。
//!
//! 本模块提供仿真内核中使用的锁机制：
//! - `KernLock`：内核全局递归锁，支持同线程可重入，用固定大小栈追踪嵌套持有者
//! - `Spin`：简单自旋锁
//! - `FlgGuard`：RAII 风格的标志守卫（stub）
//! - `GKL`：全局内核锁的静态实例

use std::sync::atomic::{AtomicBool, AtomicU64, AtomicUsize, Ordering};

/// holder_stack 的最大嵌套深度（栈容量）。
/// 实测中内核锁嵌套不会超过此值。
const HOLDER_STACK_CAP: usize = 16;

/// 内核全局递归锁。
///
/// 支持同一线程的可重入（递归）加锁。使用 `owner_tid` 追踪真正的持有者线程，
/// 因此同一线程使用不同的 `id` 值也能正确实现可重入。
/// 使用固定大小的 `holder_stack` 栈数组追踪每一层嵌套的持有者 id，
/// `leave(id)` 会校验配对。
///
/// 所有字段都是 `const fn` 兼容的原子类型，因此 `GKL` 可以直接用 `static` 初始化。
///
/// # 字段
/// - `flag`: 锁占用标志（true = 已被持有）
/// - `holder_stack`: 持有者 ID 栈（固定 16 槽的 AtomicUsize 数组），
///   用 `depth` 作为栈指针索引；`stack[depth-1]` 为栈顶（当前层），
///   `stack[0]` 为最外层
/// - `depth`: 递归深度 / 栈指针（0 = 未持有，1 = 首次获取）
/// - `owner_tid`: 持有者线程的唯一标识（u64 格式的 ThreadId），
///   用于判断同线程可重入（而非靠 holder id）
pub struct KernLock {
    /// 锁占用标志（true = 已被持有）。
    pub flag: AtomicBool,
    /// 持有者 ID 固定栈，`holder_stack[depth-1]` 为当前层持有者。
    /// 深度 0 时所有槽为 0 表示空。
    pub holder_stack: [AtomicUsize; HOLDER_STACK_CAP],
    /// 递归加锁深度 / 栈指针（0 = 未持有，1 = 首次获取）。
    pub depth: AtomicUsize,
    /// 持有者线程 ID（u64），用于判断同线程可重入。
    pub owner_tid: AtomicU64,
}
impl KernLock {
    /// 创建一个新的未上锁的 `KernLock`。
    ///
    /// 所有槽初始化为 0，可安全用于 `static` 常量初始化。
    pub const fn new() -> Self {
        Self {
            flag: AtomicBool::new(false),
            holder_stack: [
                AtomicUsize::new(0), AtomicUsize::new(0), AtomicUsize::new(0), AtomicUsize::new(0),
                AtomicUsize::new(0), AtomicUsize::new(0), AtomicUsize::new(0), AtomicUsize::new(0),
                AtomicUsize::new(0), AtomicUsize::new(0), AtomicUsize::new(0), AtomicUsize::new(0),
                AtomicUsize::new(0), AtomicUsize::new(0), AtomicUsize::new(0), AtomicUsize::new(0),
            ],
            depth: AtomicUsize::new(0),
            owner_tid: AtomicU64::new(0),
        }
    }

    /// 获取当前线程 ID 的 u64 表示，用于可重入判断。
    fn current_tid_u64() -> u64 {
        unsafe { std::mem::transmute(std::thread::current().id()) }
    }

    /// 辅助：读取栈顶的 holder（无 holder 时返回 0）。
    fn stack_top(&self) -> usize {
        let d = self.depth.load(Ordering::Relaxed);
        if d == 0 { return 0; }
        self.holder_stack[d - 1].load(Ordering::Relaxed)
    }

    /// 获取内核锁。
    ///
    /// 如果当前线程已持有该锁（通过 `owner_tid` 判断），则将 `id` 写入
    /// `holder_stack[depth]`，深度+1（可重入）。否则自旋等待直到成功获取锁，
    /// 将 `id` 写入 `holder_stack[0]` 并记录 owner_tid。
    ///
    /// 如果 `depth` 达到 `HOLDER_STACK_CAP`，panic（嵌套深度溢出）。
    ///
    /// # 参数
    /// - `id`: 调用者的标识 ID（通常为操作类型或任务 ID），传入 0 表示跳过可重入检查
    pub fn enter(&self, id: usize) {
        let cur_tid = Self::current_tid_u64();
        let owner = self.owner_tid.load(Ordering::Relaxed);
        let d = self.depth.load(Ordering::Relaxed);
        let f = self.flag.load(Ordering::Relaxed);
        let tid = format!("{:?}", std::thread::current().id());
        eprintln!(
            "[DBG] KernLock::enter id={} depth={} flag={} owner_tid={} cur_tid={} tid={}",
            id, d, f, owner, cur_tid, tid
        );
        // 同线程可重入：同一线程使用不同 id 也能正确重入
        if owner == cur_tid && id != 0 {
            assert!(d < HOLDER_STACK_CAP, "holder_stack overflow at depth={}", d);
            eprintln!(
                "[DBG] KernLock::enter REENTRANT (same thread) id={} depth:{}→{} tid={}",
                id, d, d + 1, tid
            );
            // 将 id 写入当前栈顶（即深度 d 的位置，0 索引）
            self.holder_stack[d].store(id, Ordering::Relaxed);
            self.depth.fetch_add(1, Ordering::Relaxed);
            return;
        }
        if f {
            let top = self.stack_top();
            eprintln!(
                "[DBG] KernLock::enter SPIN_WAIT id={} flag=true holder_top={} tid={}",
                id, top, tid
            );
        }
        let mut spin_cnt: u64 = 0;
        while self
            .flag
            .compare_exchange(false, true, Ordering::Acquire, Ordering::Relaxed)
            .is_err()
        {
            spin_cnt += 1;
            if spin_cnt % 10_000_000 == 1 {
                let top = self.stack_top();
                eprintln!(
                    "[DBG] KernLock::enter SPINNING id={} cnt={} holder_top={} tid={}",
                    id, spin_cnt, top, tid
                );
            }
            core::hint::spin_loop();
        }
        eprintln!(
            "[DBG] KernLock::enter ACQUIRED id={} depth:{}→1 tid={}",
            id, d, tid
        );
        // 清空栈并将 id 写入 holder_stack[0]
        for i in 0..HOLDER_STACK_CAP {
            self.holder_stack[i].store(0, Ordering::Relaxed);
        }
        self.holder_stack[0].store(id, Ordering::Relaxed);
        self.depth.store(1, Ordering::Relaxed);
        self.owner_tid.store(cur_tid, Ordering::Relaxed);
    }

    /// 释放内核锁。
    ///
    /// 接收 `id` 参数用于校验与 `enter()` 的配对：
    /// `holder_stack[depth-1]` 必须等于 `id`，否则为嵌套配对错误。
    ///
    /// 如果递归深度大于 1：清零 `holder_stack[depth-1]`，递减深度。
    /// 如果深度为 1（最后一层）：清零所有字段，释放锁。
    ///
    /// # 参数
    /// - `id`: 调用者的标识 ID，必须与对应 `enter()` 传入的 id 一致
    pub fn leave(&self, id: usize) {
        let d = self.depth.load(Ordering::Relaxed);
        let tid = format!("{:?}", std::thread::current().id());

        // 校验：holder_stack[depth-1] 必须匹配传入的 id
        let top = if d > 0 {
            self.holder_stack[d - 1].load(Ordering::Relaxed)
        } else {
            0
        };
        eprintln!(
            "[DBG] KernLock::leave id={} holder_top={} depth={} tid={}",
            id, top, d, tid
        );
        if d > 0 && top != id {
            eprintln!(
                "[DBG] KernLock::leave MISMATCH! expected id={} but holder_top={} depth={} tid={}",
                id, top, d, tid
            );
        }

        if d > 1 {
            eprintln!(
                "[DBG] KernLock::leave DECR depth:{}→{} tid={}",
                d, d - 1, tid
            );
            // 清零当前层的槽位，堆栈指针回退
            self.holder_stack[d - 1].store(0, Ordering::Relaxed);
            self.depth.store(d - 1, Ordering::Relaxed);
            return;
        }
        // 最后一层：完全释放
        eprintln!(
            "[DBG] KernLock::leave RELEASE depth:{}→0 flag→false tid={}",
            d, tid
        );
        for i in 0..HOLDER_STACK_CAP {
            self.holder_stack[i].store(0, Ordering::Relaxed);
        }
        self.depth.store(0, Ordering::Relaxed);
        self.owner_tid.store(0, Ordering::Relaxed);
        self.flag.store(false, Ordering::Release);
    }

    /// 检查锁当前是否被持有（不区分持有者）。
    pub fn held(&self) -> bool {
        eprintln!("[DBG] KernLock::held");
        self.flag.load(Ordering::Relaxed)
    }

    /// 返回栈顶的持有者标识 ID（0 表示无人持有）。
    pub fn owner(&self) -> usize {
        eprintln!("[DBG] KernLock::owner");
        self.stack_top()
    }

    /// 返回当前递归深度（0 表示未持有）。
    pub fn level(&self) -> usize {
        eprintln!("[DBG] KernLock::level");
        self.depth.load(Ordering::Relaxed)
    }

    /// 尝试获取锁，失败时立即返回 false。
    ///
    /// 如果当前线程已持有该锁（同线程可重入），则将 `id` 写入栈顶、深度+1 并返回 true。
    pub fn try_enter(&self, id: usize) -> bool {
        eprintln!("[DBG] KernLock::try_enter");
        let cur_tid = Self::current_tid_u64();
        let owner = self.owner_tid.load(Ordering::Relaxed);
        if owner == cur_tid && id != 0 {
            let d = self.depth.load(Ordering::Relaxed);
            assert!(d < HOLDER_STACK_CAP, "holder_stack overflow");
            self.holder_stack[d].store(id, Ordering::Relaxed);
            self.depth.fetch_add(1, Ordering::Relaxed);
            return true;
        }
        if self
            .flag
            .compare_exchange(false, true, Ordering::Acquire, Ordering::Relaxed)
            .is_ok()
        {
            for i in 0..HOLDER_STACK_CAP {
                self.holder_stack[i].store(0, Ordering::Relaxed);
            }
            self.holder_stack[0].store(id, Ordering::Relaxed);
            self.depth.store(1, Ordering::Relaxed);
            self.owner_tid.store(cur_tid, Ordering::Relaxed);
            true
        } else {
            false
        }
    }
}
// SAFETY: KernLock 内部仅使用原子类型，可安全跨线程使用。
unsafe impl Send for KernLock {}
unsafe impl Sync for KernLock {}

/// 全局内核锁的静态实例。
///
/// 整个内核中保护关键数据结构的全局锁。
/// 由于所有字段都是 `const fn` 兼容的原子类型（`holder_stack` 为固定大小的 `[AtomicUsize; 16]`），
/// 可以直接用 `static` 初始化，无需 `lazy_static!`。
pub static GKL: KernLock = KernLock::new();

// ── 简单自旋锁 ──

/// 简单自旋锁。
///
/// 基于 `AtomicBool` 实现的最简单的忙等待自旋锁。
/// 不支持可重入，用于保护短期持有的数据。
///
/// # 字段
/// - `v`: 锁标志（false = 未锁定，true = 已锁定）
pub struct Spin {
    /// 锁标志位。
    pub v: AtomicBool,
}
impl Spin {
    /// 创建一个新的未锁定自旋锁。
    pub const fn new() -> Self {
        Self { v: AtomicBool::new(false) }
    }

    /// 获取自旋锁，忙等待直到成功。
    pub fn acquire(&self) {
        eprintln!("[DBG] Spin::acquire");
        while self
            .v
            .compare_exchange(false, true, Ordering::Acquire, Ordering::Relaxed)
            .is_err()
        {
            core::hint::spin_loop();
        }
    }

    /// 尝试获取自旋锁，失败时立即返回 false（不等待）。
    pub fn try_acquire(&self) -> bool {
        eprintln!("[DBG] Spin::try_acquire");
        self.v
            .compare_exchange(false, true, Ordering::Acquire, Ordering::Relaxed)
            .is_ok()
    }

    /// 释放自旋锁。
    pub fn release(&self) {
        eprintln!("[DBG] Spin::release");
        self.v.store(false, Ordering::Release);
    }

    /// 检查锁当前是否被持有。
    pub fn is_held(&self) -> bool {
        eprintln!("[DBG] Spin::is_held");
        self.v.load(Ordering::Relaxed)
    }
}
// SAFETY: Spin 内部仅使用原子类型，可安全跨线程使用。
unsafe impl Send for Spin {}
unsafe impl Sync for Spin {}

// ── 标志守卫 ──

/// RAII 风格的内核标志守卫（当前为 stub 实现）。
///
/// 用于在进入/退出内核态时自动设置/清除标志。
/// 当前仅作为占位符，实际功能未实现。
pub struct FlgGuard(pub usize);
impl FlgGuard {
    /// 进入受保护区域，创建守卫实例。
    pub fn enter() -> Self {
        Self(0)
    }
}
impl Drop for FlgGuard {
    /// 离开受保护区域时自动调用。
    fn drop(&mut self) {}
}
