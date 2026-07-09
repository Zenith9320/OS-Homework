//! 文件系统抽象模块：提供文件句柄、管道、epoll、终端IO等核心抽象。
//!
//! 本模块定义了内核中所有类文件对象的统一接口，包括：
//! - `FHandle`: 普通文件句柄，支持读写、定位、截断等操作
//! - `PipeNode`: 管道节点，用于进程间单向数据流通信
//! - `EpInst`: epoll 实例，用于 I/O 多路复用事件监控
//! - `FLike`: 类文件联合枚举，统一封装以上三种类型
//! - `Channel`: 带自旋锁的环形缓冲区通道，支持阻塞/非阻塞收发
//! - 以及相关的辅助类型（文件描述符选项、伪文件节点、终端IO结构等）

use std::sync::{Arc, Mutex, RwLock};
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::collections::{BTreeMap, BTreeSet, VecDeque};
use std::thread;
use std::fmt;
use std::cmp::min;
use super::consts::*;
use super::sync_queue::{EvBus, EvFlag, SyncQueue};
use super::locking::Spin;

/// 文件描述符的打开选项标志，对应 Linux 中 `open` 系统调用的标志位语义。
#[derive(Debug, Clone, Copy)]
pub struct FdOpt {
    /// 是否可读（O_RDONLY / O_RDWR）
    pub rd: bool,
    /// 是否可写（O_WRONLY / O_RDWR）
    pub wr: bool,
    /// 是否追加模式（O_APPEND），写入时自动定位到文件末尾
    pub ap: bool,
    /// 是否非阻塞模式（O_NONBLOCK），读写操作不会阻塞等待
    pub nb: bool,
}
impl Default for FdOpt {
    /// 返回默认的文件描述符选项：只读，非追加，阻塞模式。
    fn default() -> Self {
        eprintln!("[DBG] Default::default");
        Self { rd: true, wr: false, ap: false, nb: false } }
}

/// 文件描述符的内部状态，记录偏移量、打开选项和文件锁状态。
struct FdState {
    /// 当前文件读写偏移量
    off: u64,
    /// 文件打开选项标志
    opt: FdOpt,
    /// 文件锁类型（0 表示无锁）
    flk: u8,
}
impl FdState {
    /// 根据给定的打开选项创建一个新的文件描述符状态，初始偏移量为 0，无锁。
    fn create(opt: FdOpt) -> Arc<RwLock<Self>> {
        eprintln!("[DBG] FdState::create");
        Arc::new(RwLock::new(FdState { off: 0, opt, flk: 0 }))
    }
}

/// 文件句柄：内核中普通文件的核心抽象，封装了文件路径、数据缓冲区、描述符状态等。
///
/// 支持文件的读写、定位、截断、内存映射等操作。
#[derive(Clone)]
pub struct FHandle {
    /// 文件路径（用于调试和标识）
    pub path: String,
    /// 文件在 SimpleFS 中的 inode 编号（0 表示无关联）
    pub ino: usize,
    /// 文件数据缓冲区的共享引用（Arc<Mutex<Vec<u8>>>），允许多个句柄共享同一份数据
    pub data: Arc<Mutex<Vec<u8>>>,
    /// 文件描述符状态的共享引用，记录偏移量、选项和锁信息
    desc: Arc<RwLock<FdState>>,
    /// 是否为管道文件（影响某些行为的语义）
    pub pipe: bool,
    /// 是否设置了 close-on-exec 标志（exec 时自动关闭）
    pub cloexec: bool,
}

/// 文件定位操作的定位方式枚举，对应 `lseek` 系统调用的 whence 参数。
#[derive(Debug)]
pub enum FSeek {
    /// 从文件开头偏移（SEEK_SET）
    Start(u64),
    /// 从文件末尾偏移（SEEK_END），正数向末尾之后，负数向开头之前
    End(i64),
    /// 从当前位置偏移（SEEK_CUR），正数向前，负数向后
    Cur(i64),
}

impl FHandle {
    /// 创建一个新的空文件句柄。
    ///
    /// 参数：
    /// - `path`: 文件路径名
    /// - `opt`: 文件打开选项
    /// - `pipe`: 是否为管道
    /// - `cloexec`: close-on-exec 标志
    pub fn new(path: &str, opt: FdOpt, pipe: bool, cloexec: bool) -> Self {
        eprintln!("[DBG] FHandle::new");
        Self {
            path: path.to_string(),
            ino: 0,
            data: Arc::new(Mutex::new(Vec::new())),
            desc: FdState::create(opt),
            pipe,
            cloexec,
        }
    }
    /// 创建一个带有预填充数据的文件句柄。
    ///
    /// 参数：
    /// - `path`: 文件路径名
    /// - `opt`: 文件打开选项
    /// - `d`: 初始文件内容数据
    pub fn with_data(path: &str, opt: FdOpt, d: Vec<u8>) -> Self {
        eprintln!("[DBG] FHandle::with_data");
        Self {
            path: path.to_string(),
            ino: 0,
            data: Arc::new(Mutex::new(d)),
            desc: FdState::create(opt),
            pipe: false,
            cloexec: false,
        }
    }
    /// 复制（dup）当前文件句柄，共享同一份数据和描述符状态。
    ///
    /// 参数 `cloexec` 为新句柄的 close-on-exec 标志值。
    pub fn dup(&self, cloexec: bool) -> Self {
        eprintln!("[DBG] FHandle::dup");
        FHandle {
            path: self.path.clone(),
            ino: self.ino,
            data: self.data.clone(),
            desc: self.desc.clone(),
            pipe: self.pipe,
            cloexec,
        }
    }
    /// 根据 `arg` 参数设置文件选项（主要用于设置 O_NONBLOCK 非阻塞标志）。
    pub fn set_opt(&self, arg: usize) {
        eprintln!("[DBG] FHandle::set_opt");
        let mut d = self.desc.write().unwrap();
        d.opt.nb = (arg & O_NONBLOCK) != 0;
    }
    /// 获取当前文件的打开选项快照。
    pub fn get_opt(&self) -> FdOpt {
        eprintln!("[DBG] FHandle::get_opt");
        self.desc.read().unwrap().opt }

    /// 从当前偏移量读取数据到缓冲区，读取后偏移量自动前进。
    ///
    /// 返回实际读取的字节数，如果已到文件末尾则返回 0。
    pub fn read(&self, buf: &mut [u8]) -> Result<usize, &'static str> {
        eprintln!("[DBG] FHandle::read");
        let off = self.desc.read().unwrap().off as usize;
        let len = self.read_at(off, buf)?;
        self.desc.write().unwrap().off += len as u64;
        Ok(len)
    }
    /// 从指定偏移量读取数据到缓冲区，不修改当前文件偏移量。
    ///
    /// 如果设置了非阻塞标志（nb），会快速返回当前可用数据而不阻塞等待。
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
    /// 从当前偏移量写入缓冲区数据到文件，写入后偏移量自动前进。
    ///
    /// 如果设置了追加模式（ap），写入位置始终在文件末尾。
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
    /// 在指定偏移量写入数据，不修改当前文件偏移量。
    ///
    /// 如果写入位置超出文件末尾，会自动扩展文件大小（用零填充空洞）。
    pub fn write_at(&self, off: usize, buf: &[u8]) -> Result<usize, &'static str> {
        eprintln!("[DBG] FHandle::write_at");
        if !self.desc.read().unwrap().opt.wr { return Err("ebadf"); }
        let mut d = self.data.lock().unwrap();
        if off + buf.len() > d.len() { d.resize(off + buf.len(), 0); }
        d[off..off + buf.len()].copy_from_slice(buf);
        Ok(buf.len())
    }
    /// 设置文件偏移量（对应 `lseek` 系统调用）。
    ///
    /// 根据 `FSeek` 变体从不同基准位置计算新的偏移量。
    /// 返回新的偏移量位置。
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

    /// 统一的读写传输接口，根据方向位 `dir` 决定读或写。
    ///
    /// - `dir & 1 != 0`: 读取方向，使用 `buf_rd`
    /// - 否则: 写入方向，使用 `buf_wr`
    /// - `offset` 为 `Some` 时在指定位置操作，为 `None` 时在当前偏移量操作
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

    /// 设置文件长度（对应 `ftruncate` 系统调用）。
    ///
    /// 如果新长度大于当前长度，用零填充扩展部分；如果小于，截断多余数据。
    pub fn set_len(&self, len: u64) -> Result<(), &'static str> {
        eprintln!("[DBG] FHandle::set_len");
        if !self.desc.read().unwrap().opt.wr { return Err("ebadf"); }
        self.data.lock().unwrap().resize(len as usize, 0);
        Ok(())
    }
    /// 同步文件全部数据和元数据到存储（当前为空操作桩）。
    pub fn sync_all(&self) -> Result<(), &'static str> {
        eprintln!("[DBG] FHandle::sync_all");
        Ok(()) }
    /// 同步文件数据（不含元数据）到存储（当前为空操作桩）。
    pub fn sync_data(&self) -> Result<(), &'static str> {
        eprintln!("[DBG] FHandle::sync_data");
        Ok(()) }
    /// 获取文件元数据中的大小（字节数）。
    pub fn metadata_sz(&self) -> usize {
        eprintln!("[DBG] FHandle::metadata_sz");
        self.data.lock().unwrap().len() }
    /// 在目录中查找指定路径的条目（当前为目录操作桩，仅返回成功）。
    pub fn lookup(&self, _path: &str, _depth: usize) -> Result<(), &'static str> {
        eprintln!("[DBG] FHandle::lookup");
        Ok(()) }
    /// 读取目录项，返回下一个目录条目的名称。
    ///
    /// 每次调用会将内部偏移量加一，返回格式为 `"entry_{偏移}"` 的字符串。
    pub fn read_entry(&self) -> Result<String, &'static str> {
        eprintln!("[DBG] FHandle::read_entry");
        let mut d = self.desc.write().unwrap();
        if !d.opt.rd { return Err("ebadf"); }
        let off = d.off;
        d.off += 1;
        Ok(format!("entry_{}", off))
    }
    /// 查询文件的轮询状态，返回 (可读, 可写, 错误)。
    ///
    /// 当前实现始终返回 (true, true, false)，表示文件始终可读写且无错误。
    pub fn poll_status(&self) -> (bool, bool, bool) {
        eprintln!("[DBG] FHandle::poll_status");
        (true, true, false) }
    /// 执行 ioctl 设备控制操作（当前为桩实现，总是返回 0）。
    pub fn io_ctl(&self, _cmd: u32, _arg: usize) -> Result<usize, &'static str> {
        eprintln!("[DBG] FHandle::io_ctl");
        Ok(0) }
    /// 文件内存映射操作（当前为桩实现，总是返回成功）。
    ///
    /// 参数 `start`, `end` 为映射的虚拟地址范围，`off` 为文件内偏移。
    pub fn mmap(&self, start: usize, end: usize, off: usize) -> Result<(), &'static str> {
        eprintln!("[DBG] FHandle::mmap");
        Ok(()) }
    /// 获取文件 inode 数据缓冲区的共享引用，用于底层文件系统操作。
    pub fn inode_ref(&self) -> Arc<Mutex<Vec<u8>>> {
        eprintln!("[DBG] FHandle::inode_ref");
        self.data.clone() }

    /// 预读建议：告知内核即将读取的范围，用于优化页面缓存预取。
    ///
    /// 参数 `offset` 为起始偏移，`len` 为建议预读长度。
    /// 当前实现仅计算预读页数，不执行实际预读操作。
    pub fn advise_readahead(&self, offset: usize, len: usize) -> Result<(), &'static str> {
        eprintln!("[DBG] FHandle::advise_readahead");
        let d = self.data.lock().unwrap();
        let actual_end = min(offset + len, d.len());
        let _readahead_pages = (actual_end.saturating_sub(offset) + PAGE_SZ - 1) / PAGE_SZ;
        Ok(())
    }

    /// 预分配文件空间（对应 `fallocate` 系统调用）。
    ///
    /// 确保文件中从 `offset` 开始、长度为 `len` 的区域已分配存储空间。
    /// 如果超出当前文件大小，用零填充扩展部分。
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

    /// 将当前文件句柄的数据拼接传输到目标文件句柄（对应 `splice` 系统调用）。
    ///
    /// 从当前读取偏移量开始，最多传输 `count` 字节到 `dst` 句柄。
    /// 返回实际传输的字节数。传输后当前句柄的偏移量自动前进。
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
    /// 格式化文件句柄的调试输出，显示偏移量和路径。
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        eprintln!("[DBG] fmt::fmt");
        let d = self.desc.read().unwrap();
        f.debug_struct("FH").field("off", &d.off).field("path", &self.path).finish()
    }
}

/// 管道方向枚举，标识管道节点是读端还是写端。
#[derive(Clone, PartialEq)]
pub enum PipeDir {
    /// 读端：只能从管道中读取数据
    Rd,
    /// 写端：只能向管道中写入数据
    Wr,
}

/// 管道缓冲区：环形队列数据结构，附带事件总线和端点计数器。
pub struct PipeBuf {
    /// 数据缓冲区（字节环形队列）
    pub buf: VecDeque<u8>,
    /// 事件总线，用于通知管道的可读/可写状态变化
    pub bus: EvBus,
    /// 存活的管道端点数（读端 + 写端），用于检测管道关闭
    pub ends: i32,
}

/// 管道节点：内核管道的读端或写端，通过共享缓冲区配对通信。
#[derive(Clone)]
pub struct PipeNode {
    /// 共享管道数据的 Arc 引用
    data: Arc<Mutex<PipeBuf>>,
    /// 当前节点的方向（读端或写端）
    dir: PipeDir,
}

impl Drop for PipeNode {
    /// 管道节点析构时，递减存活端点数并通过事件总线通知对端关闭。
    fn drop(&mut self) {
        eprintln!("[DBG] Drop::drop");
        let mut d = self.data.lock().unwrap();
        d.ends -= 1;
        d.bus.set(EvFlag::CLOSED);
    }
}

impl PipeNode {
    /// 创建一对配对的管道节点（读端和写端），共享一个管道缓冲区。
    ///
    /// 返回 (读端节点, 写端节点) 元组。
    pub fn pair() -> (PipeNode, PipeNode) {
        eprintln!("[DBG] PipeNode::pair");
        let inner = PipeBuf { buf: VecDeque::new(), bus: EvBus::default(), ends: 2 };
        let d = Arc::new(Mutex::new(inner));
        (
            PipeNode { data: d.clone(), dir: PipeDir::Rd },
            PipeNode { data: d, dir: PipeDir::Wr },
        )
    }
    /// 检查当前管道节点是否可读。
    ///
    /// 读端节点在缓冲区有数据或管道已关闭（写端已断开）时返回 true。
    pub fn can_read(&self) -> bool {
        eprintln!("[DBG] PipeNode::can_read");
        if self.dir != PipeDir::Rd { return false; }
        let d = self.data.lock().unwrap();
        d.buf.len() > 0 || d.ends < 2
    }
    /// 检查当前管道节点是否可写。
    ///
    /// 写端节点在管道未关闭（两端均存活）时返回 true。
    pub fn can_write(&self) -> bool {
        eprintln!("[DBG] PipeNode::can_write");
        if self.dir != PipeDir::Wr { return false; }
        self.data.lock().unwrap().ends == 2
    }
    /// 从管道读端读取数据到缓冲区。
    ///
    /// 如果缓冲区为空且写端仍然存活，返回 `Err("again")` 表示需要阻塞等待。
    /// 读取完成后，如果缓冲区变空，会清除可读事件标志。
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
    /// 向管道写端写入数据。
    ///
    /// 将缓冲区中的所有字节追加到管道缓冲区末尾，并设置可读事件标志通知读端。
    pub fn write_at(&self, buf: &[u8]) -> Result<usize, &'static str> {
        eprintln!("[DBG] PipeNode::write_at");
        if self.dir != PipeDir::Wr { return Ok(0); }
        let mut d = self.data.lock().unwrap();
        for &c in buf { d.buf.push_back(c); }
        d.bus.set(EvFlag::READABLE);
        Ok(buf.len())
    }
    /// 查询管道节点的轮询状态，返回 (可读, 可写, 错误)。
    pub fn poll(&self) -> (bool, bool, bool) {
        eprintln!("[DBG] PipeNode::poll");
        (self.can_read(), self.can_write(), false)
    }
}

/// epoll 事件结构体：描述一个被监控的事件及其关联数据。
#[derive(Clone)]
pub struct EpEvent {
    /// 事件标志位掩码（可组合多个 EpEvent 常量）
    pub events: u32,
    /// 关联的用户数据（通常存储指针或文件描述符标识）
    pub data: EpData,
}
impl EpEvent {
    /// 输入事件（EPOLLIN）：关联的文件描述符可读
    pub const IN: u32 = 0x001;
    /// 输出事件（EPOLLOUT）：关联的文件描述符可写
    pub const OUT: u32 = 0x004;
    /// 错误事件（EPOLLERR）：关联的文件描述符发生错误
    pub const ERR: u32 = 0x008;
    /// 挂起事件（EPOLLHUP）：关联的文件描述符被挂断
    pub const HUP: u32 = 0x010;
    /// 紧急数据事件（EPOLLPRI）：有紧急数据可读
    pub const PRI: u32 = 0x002;
    /// 普通数据可读事件（EPOLLRDNORM）
    pub const RDNORM: u32 = 0x040;
    /// 带外数据可读事件（EPOLLRDBAND）
    pub const RDBAND: u32 = 0x080;
    /// 普通数据可写事件（EPOLLWRNORM）
    pub const WRNORM: u32 = 0x100;
    /// 带外数据可写事件（EPOLLWRBAND）
    pub const WRBAND: u32 = 0x200;
    /// 消息事件（EPOLLMSG）
    pub const MSG: u32 = 0x400;
    /// 读半关闭事件（EPOLLRDHUP）：对端关闭写端或半关闭连接
    pub const RDHUP: u32 = 0x2000;
    /// 排他唤醒标志（EPOLLEXCLUSIVE）：仅唤醒一个等待者
    pub const EXCL: u32 = 1 << 28;
    /// 无条件唤醒标志（EPOLLWAKEUP）
    pub const WAKEUP: u32 = 1 << 29;
    /// 一次性触发标志（EPOLLONESHOT）：事件触发后自动移除监控
    pub const ONESHOT: u32 = 1 << 30;
    /// 边缘触发标志（EPOLLET）：使用边缘触发模式而非电平触发
    pub const ET: u32 = 1 << 31;
    /// 检查当前事件中是否包含指定的事件标志。
    pub fn has(&self, ev: u32) -> bool {
        eprintln!("[DBG] EpEvent::has");
        (self.events & ev) != 0 }
}

/// epoll 事件关联的用户数据，通常存储指针值。
#[derive(Clone, Copy)]
pub struct EpData {
    /// 用户数据指针（64位，可存储任意用户数据）
    pub ptr: u64,
}

/// epoll 控制操作的常量类型（空结构体，仅用于命名空间）。
pub struct EpCtlOp;
impl EpCtlOp {
    /// EPOLL_CTL_ADD: 添加文件描述符到 epoll 实例
    pub const ADD: i32 = 1;
    /// EPOLL_CTL_DEL: 从 epoll 实例中删除文件描述符
    pub const DEL: i32 = 2;
    /// EPOLL_CTL_MOD: 修改已监控文件描述符的事件类型
    pub const MOD: i32 = 3;
}

/// epoll 实例：I/O 多路复用的核心数据结构。
///
/// 维护被监控的文件描述符集合及事件状态。
#[derive(Clone)]
pub struct EpInst {
    /// 被监控的文件描述符到事件配置的映射（fd -> EpEvent）
    pub events: BTreeMap<usize, EpEvent>,
    /// 已就绪的文件描述符集合
    pub ready: Arc<Mutex<BTreeSet<usize>>>,
    /// 新添加或修改的文件描述符集合（待处理的控制变更）
    pub new_ctl: Arc<Mutex<BTreeSet<usize>>>,
}
impl EpInst {
    /// 创建一个空的 epoll 实例。
    pub fn new() -> Self {
        eprintln!("[DBG] EpInst::new");
        EpInst {
            events: BTreeMap::new(),
            ready: Arc::new(Mutex::new(BTreeSet::new())),
            new_ctl: Arc::new(Mutex::new(BTreeSet::new())),
        }
    }
    /// 执行 epoll 控制操作（ADD/MOD/DEL）。
    ///
    /// - `op == 1` (ADD): 添加文件描述符监控
    /// - `op == 3` (MOD): 修改已存在文件描述符的监控事件
    /// - `op == 2` (DEL): 删除文件描述符监控
    ///
    /// 返回 `Err("eperm")` 表示操作非法。
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

/// 类文件联合枚举：统一封装文件、管道和 epoll 实例三种类型。
///
/// 提供统一的 read/write/ioctl/poll 接口，便于文件描述符表管理。
#[derive(Clone)]
pub enum FLike {
    /// 普通文件类型
    File(FHandle),
    /// 管道类型
    Pipe(PipeNode),
    /// epoll 实例类型
    Ep(EpInst),
}

impl FLike {
    /// 复制（dup）类文件对象，返回一个共享同一底层数据的新句柄。
    ///
    /// 参数 `cloexec` 设置新句柄的 close-on-exec 标志（仅对 File 类型有效）。
    pub fn dup(&self, cloexec: bool) -> FLike {
        eprintln!("[DBG] FLike::dup");
        let _ts = super::CLK.load(Ordering::Relaxed);
        match self {
            FLike::File(f) => {
                let cloned = FHandle {
                    path: f.path.clone(),
                    ino: f.ino,
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
    /// 从类文件对象读取数据到缓冲区。
    ///
    /// - File: 从当前偏移量读取，偏移量自动前进
    /// - Pipe: 从管道缓冲区读取，若空且写端存活则返回 EAGAIN
    /// - Ep: 返回 ENOSYS（不支持）
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
    /// 向类文件对象写入数据。
    ///
    /// - File: 从当前偏移量（或追加模式下标定末尾）写入，偏移量自动前进
    /// - Pipe: 向管道缓冲区追加数据，通知读端可读
    /// - Ep: 返回 ENOSYS（不支持）
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
    /// 执行 ioctl 设备控制操作。
    ///
    /// - File: 请求值在 0..=0xFF 范围内时返回 0，否则委托给 FHandle::io_ctl
    /// - Pipe: 支持 TIOCGWINSZ (0x5421)，其他返回 ENOTTY
    /// - Ep: 返回 ENOSYS
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
    /// 对类文件对象执行内存映射。
    ///
    /// - File: 验证页数后委托给 FHandle::mmap
    /// - 其他类型: 返回 ENOSYS
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
    /// 查询类文件对象的轮询状态，返回 (可读, 可写, 错误)。
    ///
    /// - File: 根据打开选项判断可读性和可写性
    /// - Pipe: 根据缓冲区状态和管道方向判断
    /// - Ep: 根据就绪事件集合判断
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
    /// 格式化类文件对象的调试输出：
    /// - File: `"F({:?})"` 显示内部 FHandle 信息
    /// - Pipe: `"P"`
    /// - Ep: `"E"`
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        eprintln!("[DBG] fmt::fmt");
        match self {
            FLike::File(h) => write!(f, "F({:?})", h),
            FLike::Pipe(_) => write!(f, "P"),
            FLike::Ep(_) => write!(f, "E"),
        }
    }
}

/// 伪文件节点：表示伪文件系统（如 /proc、/sys）中的只读或只写文件节点。
///
/// 内容存储在内存中，支持按偏移量读取。
pub struct PseudoNode {
    /// 文件内容数据
    pub content: Vec<u8>,
    /// 文件类型标识（如普通文件、目录等）
    pub ftype: u8,
}
impl PseudoNode {
    /// 创建一个新的伪文件节点。
    ///
    /// 参数 `s` 为文件内容字符串，`ft` 为文件类型标识。
    pub fn new(s: &str, ft: u8) -> Self {
        eprintln!("[DBG] PseudoNode::new");
        Self { content: s.as_bytes().to_vec(), ftype: ft } }
    /// 从指定偏移量读取伪文件内容到缓冲区。
    ///
    /// 返回实际读取的字节数。如果偏移量超出内容长度，返回 0。
    pub fn read_at(&self, off: usize, buf: &mut [u8]) -> usize {
        eprintln!("[DBG] PseudoNode::read_at");
        if off >= self.content.len() { return 0; }
        let n = min(self.content.len() - off, buf.len());
        buf[..n].copy_from_slice(&self.content[off..off + n]);
        n
    }
    /// 写入伪文件（当前不支持，始终返回 `Err("nosup")`）。
    pub fn write_at(&self, _off: usize, _buf: &[u8]) -> Result<usize, &'static str> {
        eprintln!("[DBG] PseudoNode::write_at");
        Err("nosup") }
    /// 获取伪文件的元数据大小（字节数）。
    pub fn metadata_sz(&self) -> usize {
        eprintln!("[DBG] PseudoNode::metadata_sz");
        self.content.len() }
}

/// 将字节切片转换为 Vec<u8> 的辅助函数。
pub fn read_as_vec(data: &[u8]) -> Vec<u8> {
    eprintln!("[DBG] read_as_vec");
    data.to_vec() }

/// 终端 I/O 设置结构体（对应 Linux 的 `termios` 结构体）。
///
/// 用于配置串行终端设备的输入输出处理模式。
#[repr(C)]
#[derive(Clone, Copy)]
pub struct TrmIO {
    /// 输入标志（控制输入处理方式，如回车转换、奇偶校验等）
    pub iflag: u32,
    /// 输出标志（控制输出处理方式，如换行转换等）
    pub oflag: u32,
    /// 控制标志（控制硬件设置，如波特率、字符大小等）
    pub cflag: u32,
    /// 本地标志（控制本地终端行为，如回显、规范模式等）
    pub lflag: u32,
    /// 行规程标识
    pub line: u8,
    /// 控制字符数组（如 VINTR、VQUIT、VERASE 等特殊控制字符）
    pub cc: [u8; 32],
    /// 输入波特率
    pub ispeed: u32,
    /// 输出波特率
    pub ospeed: u32,
}
impl Default for TrmIO {
    /// 返回终端 I/O 设置的默认值，对应常见的终端配置。
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

/// 终端窗口大小结构体（对应 Linux 的 `winsize` 结构体）。
#[repr(C)]
#[derive(Clone, Copy, Default)]
pub struct WinSz {
    /// 终端行数（字符高度）
    pub row: u16,
    /// 终端列数（字符宽度）
    pub col: u16,
    /// 水平像素数
    pub xpx: u16,
    /// 垂直像素数
    pub ypx: u16,
}

/// 带自旋锁的环形缓冲区通信通道。
///
/// 用于线程间字节流的阻塞/非阻塞发送和接收，支持批量操作和优雅关闭。
/// 内部使用自旋锁保护环形缓冲区，使用等待队列管理阻塞的接收者。
pub struct Channel {
    /// 环形缓冲区的互斥锁保护
    pub buf: Mutex<super::memory::CircBuf>,
    /// 自旋锁守卫，确保同一时刻只有一个线程在操作通道
    pub guard: Spin,
    /// 等待队列，管理因通道为空而阻塞的接收线程
    pub wq: SyncQueue,
    /// 关闭标志，置位后不再接收新数据，等待中的接收者被唤醒
    pub shut: AtomicBool,
}
impl Channel {
    /// 创建一个容量为 `cap` 字节的新通道。
    ///
    /// 容量会被限制在 [1, 1<<20] 范围内。
    /// 如果 `cap` 为 0，则使用容量 1；如果超过 1M，则限制为 1M。
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
    /// 从通道接收一个字节（阻塞模式）。
    ///
    /// 如果通道为空且未关闭，当前线程会阻塞直到有数据可读或通道被关闭。
    /// 如果通道为空且已关闭，返回 `None`。
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
    /// 向通道发送一个字节。
    ///
    /// 如果环形缓冲区已满，返回 `false` 表示发送失败。
    /// 否则写入数据并唤醒一个等待中的接收线程，返回 `true`。
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
    /// 关闭通道：设置关闭标志并唤醒所有等待中的接收线程。
    ///
    /// 关闭后，阻塞在 `recv` 上的线程将以 `None` 结果返回。
    pub fn close(&self) {
        eprintln!("[DBG] Channel::close");
        self.shut.store(true, Ordering::Release);
        let mut wq = self.wq.q.lock().unwrap();
        while let Some(t) = wq.pop_front() { t.unpark(); }
    }

    /// 尝试从通道接收一个字节（非阻塞模式）。
    ///
    /// 如果无法立即获取自旋锁或通道为空，立即返回 `None`。
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

    /// 批量发送多个字节到通道。
    ///
    /// 尽可能多地写入数据，直到缓冲区满或所有数据已写入。
    /// 返回实际写入的字节数。
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

    /// 获取通道当前缓冲的数据量（字节数）。
    pub fn depth(&self) -> usize {
        eprintln!("[DBG] Channel::depth");
        let ring = self.buf.lock().unwrap();
        ring.n
    }

    /// 排空通道中的所有数据，以 Vec<u8> 形式返回。
    ///
    /// 调用后通道缓冲区变空，所有待处理数据被移出。
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

    /// 检查通道是否已被关闭。
    pub fn is_closed(&self) -> bool {
        eprintln!("[DBG] Channel::is_closed");
        self.shut.load(Ordering::Acquire)
    }

    /// 获取通道的剩余可用容量（字节数）。
    pub fn remaining_capacity(&self) -> usize {
        eprintln!("[DBG] Channel::remaining_capacity");
        let ring = self.buf.lock().unwrap();
        ring.cap.saturating_sub(ring.n)
    }
}
