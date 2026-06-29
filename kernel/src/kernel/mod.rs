#![allow(unused, dead_code, non_upper_case_globals, non_camel_case_types, unused_assignments, unused_mut)]
#![feature(renamed_spin_loop, deque_make_contiguous)]

use std::collections::BTreeMap;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::thread;

// ── Clock globals ──
pub static CLK: AtomicUsize = AtomicUsize::new(0);
pub static CLK_ALL: AtomicUsize = AtomicUsize::new(0);

pub fn wclk() -> usize {
    eprintln!("[DBG] wclk");
    CLK.load(Ordering::Relaxed) }
pub fn cclk() -> usize {
    eprintln!("[DBG] cclk");
    CLK_ALL.load(Ordering::Relaxed) }
pub fn dtk(cpu_id: usize) {
    eprintln!("[DBG] dtk");
    if cpu_id == 0 { CLK.fetch_add(1, Ordering::Relaxed); }
    CLK_ALL.fetch_add(1, Ordering::Relaxed);
}
pub fn up_ms() -> usize {
    eprintln!("[DBG] up_ms");
    wclk() * consts::USEC_TICK / 1000 }
pub fn tmr(cpu_id: usize) {
    eprintln!("[DBG] tmr");
    dtk(cpu_id); }
pub fn ser(c: u8) -> u8 {
    eprintln!("[DBG] ser");
    if c == b'\r' { b'\n' } else { c } }

pub fn yield_now_sync() {
    eprintln!("[DBG] yield_now_sync");
    thread::yield_now(); }

// ── ProcInit ──
pub struct ProcInit {
    pub args: Vec<String>,
    pub envs: Vec<String>,
    pub auxv: BTreeMap<u8, usize>,
}
impl ProcInit {
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

    pub fn total_size(&self) -> usize {
        eprintln!("[DBG] ProcInit::total_size");
        let mut sz = 0usize;
        for a in &self.args { sz += a.len() + 1; }
        for e in &self.envs { sz += e.len() + 1; }
        sz += (self.auxv.len() * 2 + 2 + self.args.len() + 1 + self.envs.len() + 1 + 1) * std::mem::size_of::<usize>();
        sz
    }
}

// ── Module declarations ──
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

// ── Flat re-exports (chaos_tests::*) ──
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
