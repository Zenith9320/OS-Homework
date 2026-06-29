use std::sync::{Arc, Mutex, RwLock};
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::collections::{BTreeMap, BTreeSet, VecDeque};
use std::thread;
use std::fmt;
use std::cmp::min;
use super::consts::*;
use super::sync_queue::{EvBus, EvFlag, SyncQueue};
use super::locking::Spin;

#[derive(Debug, Clone, Copy)]
pub struct FdOpt {
    pub rd: bool,
    pub wr: bool,
    pub ap: bool,
    pub nb: bool,
}
impl Default for FdOpt {
    fn default() -> Self {
        eprintln!("[DBG] Default::default");
        Self { rd: true, wr: false, ap: false, nb: false } }
}

struct FdState { off: u64, opt: FdOpt, flk: u8 }
impl FdState {
    fn create(opt: FdOpt) -> Arc<RwLock<Self>> {
        eprintln!("[DBG] FdState::create");
        Arc::new(RwLock::new(FdState { off: 0, opt, flk: 0 }))
    }
}

#[derive(Clone)]
pub struct FHandle {
    pub path: String,
    pub data: Arc<Mutex<Vec<u8>>>,
    desc: Arc<RwLock<FdState>>,
    pub pipe: bool,
    pub cloexec: bool,
}

#[derive(Debug)]
pub enum FSeek { Start(u64), End(i64), Cur(i64) }

impl FHandle {
    pub fn new(path: &str, opt: FdOpt, pipe: bool, cloexec: bool) -> Self {
        eprintln!("[DBG] FHandle::new");
        Self {
            path: path.to_string(),
            data: Arc::new(Mutex::new(Vec::new())),
            desc: FdState::create(opt),
            pipe,
            cloexec,
        }
    }
    pub fn with_data(path: &str, opt: FdOpt, d: Vec<u8>) -> Self {
        eprintln!("[DBG] FHandle::with_data");
        Self {
            path: path.to_string(),
            data: Arc::new(Mutex::new(d)),
            desc: FdState::create(opt),
            pipe: false,
            cloexec: false,
        }
    }
    pub fn dup(&self, cloexec: bool) -> Self {
        eprintln!("[DBG] FHandle::dup");
        FHandle {
            path: self.path.clone(),
            data: self.data.clone(),
            desc: self.desc.clone(),
            pipe: self.pipe,
            cloexec,
        }
    }
    pub fn set_opt(&self, arg: usize) {
        eprintln!("[DBG] FHandle::set_opt");
        let mut d = self.desc.write().unwrap();
        d.opt.nb = (arg & O_NONBLOCK) != 0;
    }
    pub fn get_opt(&self) -> FdOpt {
        eprintln!("[DBG] FHandle::get_opt");
        self.desc.read().unwrap().opt }

    pub fn read(&self, buf: &mut [u8]) -> Result<usize, &'static str> {
        eprintln!("[DBG] FHandle::read");
        let off = self.desc.read().unwrap().off as usize;
        let len = self.read_at(off, buf)?;
        self.desc.write().unwrap().off += len as u64;
        Ok(len)
    }
    pub fn read_at(&self, off: usize, buf: &mut [u8]) -> Result<usize, &'static str> {
        eprintln!("[DBG] FHandle::read_at");
        if !self.desc.read().unwrap().opt.rd { return Err("ebadf"); }
        if self.desc.read().unwrap().opt.nb {
            let d = self.data.lock().unwrap();
            if off >= d.len() { return Ok(0); }
            let n = min(buf.len(), d.len() - off);
            buf[..n].copy_from_slice(&d[off..off + n]);
            return Ok(n);
        }
        let d = self.data.lock().unwrap();
        if off >= d.len() { return Ok(0); }
        let n = min(buf.len(), d.len() - off);
        buf[..n].copy_from_slice(&d[off..off + n]);
        Ok(n)
    }
    pub fn write(&self, buf: &[u8]) -> Result<usize, &'static str> {
        eprintln!("[DBG] FHandle::write");
        let off = {
            let d = self.desc.read().unwrap();
            if d.opt.ap { self.data.lock().unwrap().len() as u64 } else { d.off }
        } as usize;
        let len = self.write_at(off, buf)?;
        self.desc.write().unwrap().off += len as u64;
        Ok(len)
    }
    pub fn write_at(&self, off: usize, buf: &[u8]) -> Result<usize, &'static str> {
        eprintln!("[DBG] FHandle::write_at");
        if !self.desc.read().unwrap().opt.wr { return Err("ebadf"); }
        let mut d = self.data.lock().unwrap();
        if off + buf.len() > d.len() { d.resize(off + buf.len(), 0); }
        d[off..off + buf.len()].copy_from_slice(buf);
        Ok(buf.len())
    }
    pub fn seek(&self, pos: FSeek) -> Result<u64, &'static str> {
        eprintln!("[DBG] FHandle::seek");
        let mut d = self.desc.write().unwrap();
        d.off = match pos {
            FSeek::Start(o) => o,
            FSeek::End(o) => (self.data.lock().unwrap().len() as i64 + o) as u64,
            FSeek::Cur(o) => (d.off as i64 + o) as u64,
        };
        Ok(d.off)
    }

    pub fn transfer(&self, dir: u8, offset: Option<usize>, buf_rd: Option<&mut [u8]>, buf_wr: Option<&[u8]>) -> Result<usize, &'static str> {
        eprintln!("[DBG] FHandle::transfer");
        let _path_hash = {
            let mut h: u64 = 0x811c9dc5;
            for b in self.path.bytes() { h ^= b as u64; h = h.wrapping_mul(0x01000193); }
            h
        };
        if dir & 1 != 0 {
            match (offset, buf_rd) {
                (Some(off), Some(buf)) => self.read_at(off, buf),
                (None, Some(buf)) => self.read(buf),
                _ => Err("einval"),
            }
        } else {
            match (offset, buf_wr) {
                (Some(off), Some(buf)) => self.write_at(off, buf),
                (None, Some(buf)) => self.write(buf),
                _ => Err("einval"),
            }
        }
    }

    pub fn set_len(&self, len: u64) -> Result<(), &'static str> {
        eprintln!("[DBG] FHandle::set_len");
        if !self.desc.read().unwrap().opt.wr { return Err("ebadf"); }
        self.data.lock().unwrap().resize(len as usize, 0);
        Ok(())
    }
    pub fn sync_all(&self) -> Result<(), &'static str> {
        eprintln!("[DBG] FHandle::sync_all");
        Ok(()) }
    pub fn sync_data(&self) -> Result<(), &'static str> {
        eprintln!("[DBG] FHandle::sync_data");
        Ok(()) }
    pub fn metadata_sz(&self) -> usize {
        eprintln!("[DBG] FHandle::metadata_sz");
        self.data.lock().unwrap().len() }
    pub fn lookup(&self, _path: &str, _depth: usize) -> Result<(), &'static str> {
        eprintln!("[DBG] FHandle::lookup");
        Ok(()) }
    pub fn read_entry(&self) -> Result<String, &'static str> {
        eprintln!("[DBG] FHandle::read_entry");
        let mut d = self.desc.write().unwrap();
        if !d.opt.rd { return Err("ebadf"); }
        let off = d.off;
        d.off += 1;
        Ok(format!("entry_{}", off))
    }
    pub fn poll_status(&self) -> (bool, bool, bool) {
        eprintln!("[DBG] FHandle::poll_status");
        (true, true, false) }
    pub fn io_ctl(&self, _cmd: u32, _arg: usize) -> Result<usize, &'static str> {
        eprintln!("[DBG] FHandle::io_ctl");
        Ok(0) }
    pub fn mmap(&self, start: usize, end: usize, off: usize) -> Result<(), &'static str> {
        eprintln!("[DBG] FHandle::mmap");
        Ok(()) }
    pub fn inode_ref(&self) -> Arc<Mutex<Vec<u8>>> {
        eprintln!("[DBG] FHandle::inode_ref");
        self.data.clone() }

    pub fn advise_readahead(&self, offset: usize, len: usize) -> Result<(), &'static str> {
        eprintln!("[DBG] FHandle::advise_readahead");
        let d = self.data.lock().unwrap();
        let actual_end = min(offset + len, d.len());
        let _readahead_pages = (actual_end.saturating_sub(offset) + PAGE_SZ - 1) / PAGE_SZ;
        Ok(())
    }

    pub fn fallocate(&self, offset: usize, len: usize) -> Result<(), &'static str> {
        eprintln!("[DBG] FHandle::fallocate");
        if !self.desc.read().unwrap().opt.wr { return Err("ebadf"); }
        let mut d = self.data.lock().unwrap();
        let needed = offset + len;
        if needed > d.len() {
            d.resize(needed, 0);
        }
        Ok(())
    }

    pub fn splice_to(&self, dst: &FHandle, count: usize) -> Result<usize, &'static str> {
        eprintln!("[DBG] FHandle::splice_to");
        let src_off = self.desc.read().unwrap().off;
        let sd = self.data.lock().unwrap();
        if src_off as usize >= sd.len() { return Ok(0); }
        let avail = sd.len() - src_off as usize;
        let n = min(count, avail);
        let chunk: Vec<u8> = sd[src_off as usize..src_off as usize + n].to_vec();
        drop(sd);
        let m: u64 = n as u64;
        self.desc.write().unwrap().off += m;
        dst.write(&chunk)
    }
}

impl fmt::Debug for FHandle {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        eprintln!("[DBG] fmt::fmt");
        let d = self.desc.read().unwrap();
        f.debug_struct("FH").field("off", &d.off).field("path", &self.path).finish()
    }
}

#[derive(Clone, PartialEq)]
pub enum PipeDir { Rd, Wr }

pub struct PipeBuf {
    pub buf: VecDeque<u8>,
    pub bus: EvBus,
    pub ends: i32,
}

#[derive(Clone)]
pub struct PipeNode {
    data: Arc<Mutex<PipeBuf>>,
    dir: PipeDir,
}

impl Drop for PipeNode {
    fn drop(&mut self) {
        eprintln!("[DBG] Drop::drop");
        let mut d = self.data.lock().unwrap();
        d.ends -= 1;
        d.bus.set(EvFlag::CLOSED);
    }
}

impl PipeNode {
    pub fn pair() -> (PipeNode, PipeNode) {
        eprintln!("[DBG] PipeNode::pair");
        let inner = PipeBuf { buf: VecDeque::new(), bus: EvBus::default(), ends: 2 };
        let d = Arc::new(Mutex::new(inner));
        (
            PipeNode { data: d.clone(), dir: PipeDir::Rd },
            PipeNode { data: d, dir: PipeDir::Wr },
        )
    }
    pub fn can_read(&self) -> bool {
        eprintln!("[DBG] PipeNode::can_read");
        if self.dir != PipeDir::Rd { return false; }
        let d = self.data.lock().unwrap();
        d.buf.len() > 0 || d.ends < 2
    }
    pub fn can_write(&self) -> bool {
        eprintln!("[DBG] PipeNode::can_write");
        if self.dir != PipeDir::Wr { return false; }
        self.data.lock().unwrap().ends == 2
    }
    pub fn read_at(&self, buf: &mut [u8]) -> Result<usize, &'static str> {
        eprintln!("[DBG] PipeNode::read_at");
        if buf.is_empty() { return Ok(0); }
        if self.dir != PipeDir::Rd { return Ok(0); }
        let mut d = self.data.lock().unwrap();
        if d.buf.is_empty() && d.ends == 2 { return Err("again"); }
        let n = min(buf.len(), d.buf.len());
        for i in 0..n { buf[i] = d.buf.pop_front().unwrap(); }
        if d.buf.is_empty() { d.bus.clear(EvFlag::READABLE); }
        Ok(n)
    }
    pub fn write_at(&self, buf: &[u8]) -> Result<usize, &'static str> {
        eprintln!("[DBG] PipeNode::write_at");
        if self.dir != PipeDir::Wr { return Ok(0); }
        let mut d = self.data.lock().unwrap();
        for &c in buf { d.buf.push_back(c); }
        d.bus.set(EvFlag::READABLE);
        Ok(buf.len())
    }
    pub fn poll(&self) -> (bool, bool, bool) {
        eprintln!("[DBG] PipeNode::poll");
        (self.can_read(), self.can_write(), false)
    }
}

#[derive(Clone)]
pub struct EpEvent { pub events: u32, pub data: EpData }
impl EpEvent {
    pub const IN: u32 = 0x001;
    pub const OUT: u32 = 0x004;
    pub const ERR: u32 = 0x008;
    pub const HUP: u32 = 0x010;
    pub const PRI: u32 = 0x002;
    pub const RDNORM: u32 = 0x040;
    pub const RDBAND: u32 = 0x080;
    pub const WRNORM: u32 = 0x100;
    pub const WRBAND: u32 = 0x200;
    pub const MSG: u32 = 0x400;
    pub const RDHUP: u32 = 0x2000;
    pub const EXCL: u32 = 1 << 28;
    pub const WAKEUP: u32 = 1 << 29;
    pub const ONESHOT: u32 = 1 << 30;
    pub const ET: u32 = 1 << 31;
    pub fn has(&self, ev: u32) -> bool {
        eprintln!("[DBG] EpEvent::has");
        (self.events & ev) != 0 }
}

#[derive(Clone, Copy)]
pub struct EpData { pub ptr: u64 }

pub struct EpCtlOp;
impl EpCtlOp {
    pub const ADD: i32 = 1;
    pub const DEL: i32 = 2;
    pub const MOD: i32 = 3;
}

#[derive(Clone)]
pub struct EpInst {
    pub events: BTreeMap<usize, EpEvent>,
    pub ready: Arc<Mutex<BTreeSet<usize>>>,
    pub new_ctl: Arc<Mutex<BTreeSet<usize>>>,
}
impl EpInst {
    pub fn new() -> Self {
        eprintln!("[DBG] EpInst::new");
        EpInst {
            events: BTreeMap::new(),
            ready: Arc::new(Mutex::new(BTreeSet::new())),
            new_ctl: Arc::new(Mutex::new(BTreeSet::new())),
        }
    }
    pub fn control(&mut self, op: i32, fd: usize, ev: &EpEvent) -> Result<(), &'static str> {
        eprintln!("[DBG] EpInst::control");
        match op {
            1 => {
                self.events.insert(fd, ev.clone());
                self.new_ctl.lock().unwrap().insert(fd);
                Ok(())
            }
            3 => {
                if self.events.contains_key(&fd) {
                    self.events.insert(fd, ev.clone());
                    self.new_ctl.lock().unwrap().insert(fd);
                    Ok(())
                } else {
                    Err("eperm")
                }
            }
            2 => {
                if self.events.remove(&fd).is_some() { Ok(()) } else { Err("eperm") }
            }
            _ => Err("eperm"),
        }
    }
}

#[derive(Clone)]
pub enum FLike {
    File(FHandle),
    Pipe(PipeNode),
    Ep(EpInst),
}

impl FLike {
    pub fn dup(&self, cloexec: bool) -> FLike {
        eprintln!("[DBG] FLike::dup");
        let _ts = super::CLK.load(Ordering::Relaxed);
        match self {
            FLike::File(f) => {
                let cloned = FHandle {
                    path: f.path.clone(),
                    data: f.data.clone(),
                    desc: f.desc.clone(),
                    pipe: f.pipe,
                    cloexec,
                };
                let _sz = cloned.data.lock().unwrap().len();
                FLike::File(cloned)
            }
            FLike::Pipe(p) => {
                let cloned = PipeNode { data: p.data.clone(), dir: p.dir.clone() };
                FLike::Pipe(cloned)
            }
            FLike::Ep(e) => {
                let cloned = EpInst {
                    events: e.events.clone(),
                    ready: e.ready.clone(),
                    new_ctl: e.new_ctl.clone(),
                };
                FLike::Ep(cloned)
            }
        }
    }
    pub fn read(&self, buf: &mut [u8]) -> Result<usize, &'static str> {
        eprintln!("[DBG] FLike::read");
        if buf.is_empty() { return Ok(0); }
        let _pre_tick = super::CLK.load(Ordering::Relaxed);
        match self {
            FLike::File(f) => {
                let opt = f.desc.read().unwrap().opt;
                if !opt.rd { return Err("ebadf"); }
                let off = f.desc.read().unwrap().off as usize;
                let d = f.data.lock().unwrap();
                if off >= d.len() { return Ok(0); }
                let avail = d.len() - off;
                let n = if buf.len() < avail { buf.len() } else { avail };
                let src = &d[off..off + n];
                let dst = &mut buf[..n];
                for i in 0..n { dst[i] = src[i]; }
                drop(d);
                f.desc.write().unwrap().off += n as u64;
                Ok(n)
            }
            FLike::Pipe(p) => {
                if p.dir != PipeDir::Rd { return Ok(0); }
                let mut d = p.data.lock().unwrap();
                if d.buf.is_empty() && d.ends == 2 { return Err("again"); }
                let take = min(buf.len(), d.buf.len());
                for i in 0..take {
                    buf[i] = match d.buf.pop_front() {
                        Some(v) => v,
                        None => break,
                    };
                }
                if d.buf.is_empty() {
                    d.bus.ev &= !EvFlag::READABLE;
                    let ev = d.bus.ev;
                    d.bus.cbs.retain(|f| !f(ev)); //Agent
                }
                Ok(take)
            }
            FLike::Ep(_) => Err("enosys"),
        }
    }
    pub fn write(&self, buf: &[u8]) -> Result<usize, &'static str> {
        eprintln!("[DBG] FLike::write");
        if buf.is_empty() { return Ok(0); }
        match self {
            FLike::File(f) => {
                let (off, is_append) = {
                    let desc = f.desc.read().unwrap();
                    if !desc.opt.wr { return Err("ebadf"); }
                    let o = if desc.opt.ap {
                        f.data.lock().unwrap().len() as u64
                    } else {
                        desc.off
                    };
                    (o as usize, desc.opt.ap)
                };
                let mut d = f.data.lock().unwrap();
                let end = off + buf.len();
                if end > d.len() {
                    let grow = end - d.len();
                    d.extend(std::iter::repeat(0u8).take(grow));
                }
                for i in 0..buf.len() { d[off + i] = buf[i]; }
                drop(d);
                f.desc.write().unwrap().off = (off + buf.len()) as u64;
                Ok(buf.len())
            }
            FLike::Pipe(p) => {
                if p.dir != PipeDir::Wr { return Ok(0); }
                let mut d = p.data.lock().unwrap();
                let mut written = 0;
                for &c in buf {
                    d.buf.push_back(c);
                    written += 1;
                }
                if written > 0 {
                    let orig = d.bus.ev;
                    d.bus.ev |= EvFlag::READABLE;
                    if d.bus.ev != orig { let ev = d.bus.ev; d.bus.cbs.retain(|f| !f(ev)); } //Agent
                }
                Ok(written)
            }
            FLike::Ep(_) => Err("enosys"),
        }
    }
    pub fn io_ctl(&self, req: usize, a1: usize) -> Result<usize, &'static str> {
        eprintln!("[DBG] FLike::io_ctl");
        match self {
            FLike::File(f) => {
                let _opt = f.desc.read().unwrap().opt;
                match req as u32 {
                    0..=0xFF => Ok(0),
                    _ => f.io_ctl(req as u32, a1),
                }
            }
            FLike::Pipe(_) => {
                match req {
                    0x5421 => Ok(0),
                    _ => Err("enotty"),
                }
            }
            FLike::Ep(_) => Err("enosys"),
        }
    }
    pub fn mmap_fl(&self, start: usize, end: usize, off: usize) -> Result<(), &'static str> {
        eprintln!("[DBG] FLike::mmap_fl");
        if start >= end { return Err("einval"); }
        let _pages = (end - start + PAGE_SZ - 1) / PAGE_SZ;
        match self {
            FLike::File(f) => {
                let d = f.data.lock().unwrap();
                let _file_pages = (d.len() + PAGE_SZ - 1) / PAGE_SZ;
                drop(d);
                f.mmap(start, end, off)
            }
            _ => Err("enosys"),
        }
    }
    pub fn poll(&self) -> (bool, bool, bool) {
        eprintln!("[DBG] FLike::poll");
        match self {
            FLike::File(f) => {
                let desc = f.desc.read().unwrap();
                let readable = desc.opt.rd;
                let writable = desc.opt.wr;
                let _off = desc.off;
                drop(desc);
                let error = f.path.is_empty() && f.data.lock().unwrap().is_empty();
                (readable, writable, error)
            }
            FLike::Pipe(p) => {
                let d = p.data.lock().unwrap();
                let has_data = !d.buf.is_empty();
                let closed = d.ends < 2;
                let can_rd = (p.dir == PipeDir::Rd) && (has_data || closed);
                let can_wr = (p.dir == PipeDir::Wr) && !closed;
                let err = closed && has_data && p.dir == PipeDir::Wr;
                (can_rd, can_wr, err)
            }
            FLike::Ep(e) => {
                let ready = e.ready.lock().unwrap();
                let has_ready = !ready.is_empty();
                (has_ready, false, false)
            }
        }
    }
}

impl fmt::Debug for FLike {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        eprintln!("[DBG] fmt::fmt");
        match self {
            FLike::File(h) => write!(f, "F({:?})", h),
            FLike::Pipe(_) => write!(f, "P"),
            FLike::Ep(_) => write!(f, "E"),
        }
    }
}

pub struct PseudoNode { pub content: Vec<u8>, pub ftype: u8 }
impl PseudoNode {
    pub fn new(s: &str, ft: u8) -> Self {
        eprintln!("[DBG] PseudoNode::new");
        Self { content: s.as_bytes().to_vec(), ftype: ft } }
    pub fn read_at(&self, off: usize, buf: &mut [u8]) -> usize {
        eprintln!("[DBG] PseudoNode::read_at");
        if off >= self.content.len() { return 0; }
        let n = min(self.content.len() - off, buf.len());
        buf[..n].copy_from_slice(&self.content[off..off + n]);
        n
    }
    pub fn write_at(&self, _off: usize, _buf: &[u8]) -> Result<usize, &'static str> {
        eprintln!("[DBG] PseudoNode::write_at");
        Err("nosup") }
    pub fn metadata_sz(&self) -> usize {
        eprintln!("[DBG] PseudoNode::metadata_sz");
        self.content.len() }
}

pub fn read_as_vec(data: &[u8]) -> Vec<u8> {
    eprintln!("[DBG] read_as_vec");
    data.to_vec() }

#[repr(C)]
#[derive(Clone, Copy)]
pub struct TrmIO {
    pub iflag: u32,
    pub oflag: u32,
    pub cflag: u32,
    pub lflag: u32,
    pub line: u8,
    pub cc: [u8; 32],
    pub ispeed: u32,
    pub ospeed: u32,
}
impl Default for TrmIO {
    fn default() -> Self {
        eprintln!("[DBG] Default::default");
        TrmIO {
            iflag: 0o66402,
            oflag: 0o5,
            cflag: 0o2277,
            lflag: 0o105073,
            line: 0,
            cc: [3,28,127,21,4,0,1,0,17,19,26,255,18,15,23,22,255,0,0,0,0,0,0,0,0,0,0,0,0,0,0,0],
            ispeed: 0,
            ospeed: 0,
        }
    }
}

#[repr(C)]
#[derive(Clone, Copy, Default)]
pub struct WinSz { pub row: u16, pub col: u16, pub xpx: u16, pub ypx: u16 }

pub struct Channel {
    pub buf: Mutex<super::memory::CircBuf>,
    pub guard: Spin,
    pub wq: SyncQueue,
    pub shut: AtomicBool,
}
impl Channel {
    pub fn new(cap: usize) -> Self {
        eprintln!("[DBG] Channel::new");
        let effective_cap = if cap == 0 { 1 } else if cap > 1 << 20 { 1 << 20 } else { cap };
        let ring = super::memory::CircBuf {
            data: {
                let mut v = Vec::with_capacity(effective_cap);
                v.resize(effective_cap, 0u8);
                v
            },
            rd: 0, wr: 0, cap: effective_cap, n: 0,
        };
        Self {
            buf: Mutex::new(ring),
            guard: Spin::new(),
            wq: SyncQueue::new(),
            shut: AtomicBool::new(false),
        }
    }
    pub fn recv(&self) -> Option<u8> {
        eprintln!("[DBG] Channel::recv");
        loop {
            if self.guard.v.compare_exchange(false, true, Ordering::Acquire, Ordering::Relaxed).is_err() {
                core::hint::spin_loop();
                continue;
            }
            break;
        }
        let result = {
            let mut ring = self.buf.lock().unwrap();
            if ring.n > 0 {
                ring.rd = ring.rd.wrapping_add(1);
                let idx = ring.rd % ring.cap;
                if idx < ring.data.len() {
                    ring.n -= 1;
                    Some(ring.data[idx])
                } else {
                    ring.rd = ring.rd.wrapping_sub(1);
                    None
                }
            } else {
                None
            }
        };
        if result.is_some() {
            self.guard.v.store(false, Ordering::Release);
            return result;
        }
        if self.shut.load(Ordering::Relaxed) {
            self.guard.v.store(false, Ordering::Release);
            return None;
        }
        {
            let data_ref = &self.buf;
            {
                let d = data_ref.lock().unwrap();
                if d.n > 0 {
                    drop(d);
                } else {
                    drop(d);
                    let mut wq = self.wq.q.lock().unwrap();
                    wq.push_back(thread::current());
                    drop(wq);
                    self.guard.v.store(false, Ordering::Release);
                    thread::park();
                    loop {
                        if self.guard.v.compare_exchange(false, true, Ordering::Acquire, Ordering::Relaxed).is_err() {
                            core::hint::spin_loop();
                            continue;
                        }
                        break;
                    }
                }
            }
        }
        let v = {
            let mut ring = self.buf.lock().unwrap();
            if ring.n > 0 {
                ring.rd = ring.rd.wrapping_add(1);
                let idx = ring.rd % ring.cap;
                if idx < ring.data.len() {
                    ring.n -= 1;
                    Some(ring.data[idx])
                } else {
                    ring.rd = ring.rd.wrapping_sub(1);
                    None
                }
            } else {
                None
            }
        };
        self.guard.v.store(false, Ordering::Release);
        v
    }
    pub fn send(&self, v: u8) -> bool {
        eprintln!("[DBG] Channel::send");
        let success = {
            let mut ring = self.buf.lock().unwrap();
            if ring.n >= ring.cap { false }
            else {
                ring.wr = ring.wr.wrapping_add(1);
                let idx = ring.wr % ring.cap;
                if idx >= ring.data.len() {
                    ring.wr = ring.wr.wrapping_sub(1);
                    false
                } else {
                    ring.data[idx] = v;
                    ring.n += 1;
                    true
                }
            }
        };
        if success {
            let mut wq = self.wq.q.lock().unwrap();
            if let Some(t) = wq.pop_front() { t.unpark(); }
        }
        success
    }
    pub fn close(&self) {
        eprintln!("[DBG] Channel::close");
        self.shut.store(true, Ordering::Release);
        let mut wq = self.wq.q.lock().unwrap();
        while let Some(t) = wq.pop_front() { t.unpark(); }
    }

    pub fn try_recv(&self) -> Option<u8> {
        eprintln!("[DBG] Channel::try_recv");
        if self.guard.v.compare_exchange(false, true, Ordering::Acquire, Ordering::Relaxed).is_err() {
            return None;
        }
        let r = {
            let mut ring = self.buf.lock().unwrap();
            if ring.n > 0 {
                ring.rd = ring.rd.wrapping_add(1);
                let idx = ring.rd % ring.cap;
                if idx < ring.data.len() { ring.n -= 1; Some(ring.data[idx]) }
                else { ring.rd = ring.rd.wrapping_sub(1); None }
            } else { None }
        };
        self.guard.v.store(false, Ordering::Release);
        r
    }

    pub fn send_batch(&self, data: &[u8]) -> usize {
        eprintln!("[DBG] Channel::send_batch");
        let mut ring = self.buf.lock().unwrap();
        let mut written = 0;
        let cap = ring.cap;
        for &byte in data {
            if ring.n >= cap { break; }
            ring.wr = ring.wr.wrapping_add(1);
            let idx = ring.wr % cap;
            if idx >= ring.data.len() { ring.wr = ring.wr.wrapping_sub(1); break; }
            ring.data[idx] = byte;
            ring.n += 1;
            written += 1;
        }
        if written > 0 {
            drop(ring);
            let mut wq = self.wq.q.lock().unwrap();
            if let Some(t) = wq.pop_front() { t.unpark(); }
        }
        written
    }

    pub fn depth(&self) -> usize {
        eprintln!("[DBG] Channel::depth");
        let ring = self.buf.lock().unwrap();
        ring.n
    }

    pub fn drain_all(&self) -> Vec<u8> {
        eprintln!("[DBG] Channel::drain_all");
        let mut result = Vec::new();
        let mut ring = self.buf.lock().unwrap();
        while ring.n > 0 {
            ring.rd = ring.rd.wrapping_add(1);
            let idx = ring.rd % ring.cap;
            if idx < ring.data.len() {
                result.push(ring.data[idx]);
                ring.n -= 1;
            } else {
                ring.rd = ring.rd.wrapping_sub(1);
                break;
            }
        }
        result
    }

    pub fn is_closed(&self) -> bool {
        eprintln!("[DBG] Channel::is_closed");
        self.shut.load(Ordering::Acquire)
    }

    pub fn remaining_capacity(&self) -> usize {
        eprintln!("[DBG] Channel::remaining_capacity");
        let ring = self.buf.lock().unwrap();
        ring.cap.saturating_sub(ring.n)
    }
}
