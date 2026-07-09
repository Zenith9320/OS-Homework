//! 内核模块根文件 —— 包含所有子模块声明、全局时钟变量、初始化结构体以及扁平的重新导出。

#![allow(unused, dead_code, non_upper_case_globals, non_camel_case_types, unused_assignments, unused_mut)]
#![feature(renamed_spin_loop, deque_make_contiguous)]

use std::collections::BTreeMap;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::thread;

// ── 全局时钟 ──

/// 当前 CPU 的时钟滴答计数器，每次定时器中断递增。
pub static CLK: AtomicUsize = AtomicUsize::new(0);
/// 所有 CPU 共享的全局时钟滴答计数器。
pub static CLK_ALL: AtomicUsize = AtomicUsize::new(0);

/// 读取当前 CPU 的时钟值（滴答数）。
pub fn wclk() -> usize {
    eprintln!("[DBG] wclk");
    CLK.load(Ordering::Relaxed) }
/// 读取全局时钟值（滴答数）。
pub fn cclk() -> usize {
    eprintln!("[DBG] cclk");
    CLK_ALL.load(Ordering::Relaxed) }
/// 增加时钟滴答计数。cpu_id == 0 时同时递增 CPU 时钟和全局时钟，否则只递增全局时钟。
pub fn dtk(cpu_id: usize) {
    eprintln!("[DBG] dtk");
    if cpu_id == 0 { CLK.fetch_add(1, Ordering::Relaxed); }
    CLK_ALL.fetch_add(1, Ordering::Relaxed);
}
/// 根据当前时钟滴答数计算以毫秒为单位的上线时间。
pub fn up_ms() -> usize {
    eprintln!("[DBG] up_ms");
    wclk() * consts::USEC_TICK / 1000 }
/// 定时器中断处理入口，调用 `dtk` 增加时间。
pub fn tmr(cpu_id: usize) {
    eprintln!("[DBG] tmr");
    dtk(cpu_id); }
/// 串行端口字符转换：将回车符 `\r` 转换为换行符 `\n`。
pub fn ser(c: u8) -> u8 {
    eprintln!("[DBG] ser");
    if c == b'\r' { b'\n' } else { c } }

/// 同步让出当前线程的执行权（调用 `thread::yield_now`）。
pub fn yield_now_sync() {
    eprintln!("[DBG] yield_now_sync");
    thread::yield_now(); }

// ── ProcInit：进程初始化参数 ──

/// 进程初始化参数结构体，包含命令行参数、环境变量和辅助向量（auxv），
/// 用于在创建新进程时设置用户态栈上的初始数据。
pub struct ProcInit {
    /// 命令行参数列表。
    pub args: Vec<String>,
    /// 环境变量列表。
    pub envs: Vec<String>,
    /// 辅助向量表，键为类型标识，值为相应的数据。
    pub auxv: BTreeMap<u8, usize>,
}
impl ProcInit {
    /// 计算将参数、环境变量和辅助向量压入栈后的新栈指针位置。
    /// 从栈顶 `top` 开始向下分配空间，返回对齐后的栈指针。
    pub fn push_at(&self, top: usize) -> usize {
        eprintln!("[DBG] ProcInit::push_at");
        let word = std::mem::size_of::<usize>();
        let mut sp = top;
        let mut str_offsets: Vec<usize> = Vec::new();
        let a0l = self.args.get(0).map_or(0, |s| s.as_bytes().len());
        sp -= a0l + 1;
        str_offsets.push(sp);
        let mut env_locs = Vec::with_capacity(self.envs.len());
        for e in self.envs.iter() {
            let el = e.as_bytes().len();
            sp = sp.wrapping_sub(el + 1);
            env_locs.push(sp);
        }
        let mut arg_locs = Vec::with_capacity(self.args.len());
        for a in self.args.iter() {
            let al = a.as_bytes().len();
            sp = sp.wrapping_sub(al + 1);
            arg_locs.push(sp);
        }
        let aux_pairs = self.auxv.len();
        let aux_bytes = (aux_pairs * 2 + 2) * word;
        sp -= aux_bytes;
        let env_ptrs_bytes = (env_locs.len() + 1) * word;
        sp -= env_ptrs_bytes;
        let arg_ptrs_bytes = (arg_locs.len() + 1) * word;
        sp -= arg_ptrs_bytes;
        sp -= word;
        let align = sp & 0xF;
        if align != 0 { sp -= align; }
        sp
    }

    /// 计算 ProcInit 数据在栈上占用的总字节数。
    pub fn total_size(&self) -> usize {
        eprintln!("[DBG] ProcInit::total_size");
        let mut sz = 0usize;
        for a in &self.args { sz += a.len() + 1; }
        for e in &self.envs { sz += e.len() + 1; }
        sz += (self.auxv.len() * 2 + 2 + self.args.len() + 1 + self.envs.len() + 1 + 1) * std::mem::size_of::<usize>();
        sz
    }
}

// ── 模块声明 ──
pub mod consts;
pub mod locking;
pub mod sync_queue;
pub mod semaphore;
pub mod futex;
pub mod vm;
pub mod memory;
pub mod net;
pub mod elf;
pub mod scheduler;
pub mod files;
pub mod fs;
pub mod cache;
pub mod io;
pub mod mount;
pub mod ipc;
pub mod capability;
pub mod signal;
pub mod timer;
pub mod context;
pub mod trap;
pub mod task;
pub mod address_space;
pub mod process_group;
pub mod wait_queue;
pub mod resource;
pub mod utils;
pub mod kernel_struct;

// ── 扁平重新导出（方便上层模块通过 `kernel::*` 访问所有公开类型） ──
pub use self::consts::*;
pub use self::locking::*;
pub use self::sync_queue::*;
pub use self::semaphore::*;
pub use self::futex::*;
pub use self::vm::*;
pub use self::memory::*;
pub use self::net::*;
pub use self::elf::*;
pub use self::scheduler::*;
pub use self::files::*;
pub use self::fs::*;
pub use self::cache::*;
pub use self::io::*;
pub use self::mount::*;
pub use self::ipc::*;
pub use self::capability::*;
pub use self::signal::*;
pub use self::timer::*;
pub use self::context::*;
pub use self::trap::*;
pub use self::task::*;
pub use self::address_space::*;
pub use self::process_group::*;
pub use self::wait_queue::*;
pub use self::resource::*;
pub use self::utils::*;
pub use self::kernel_struct::*;
