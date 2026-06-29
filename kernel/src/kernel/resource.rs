//! 资源限制模块 —— 定义进程资源上限结构体，支持文件描述符、线程数、
//! 栈大小、数据段大小等限制的检查和继承。

use super::consts::{KHEAP_SZ, USR_STK_SZ};

/// 资源限制结构体，定义了针对单个进程的各种系统资源上限。
///
/// 模仿 Linux 的 `rlimit` 机制，用于控制进程对系统资源的消耗。
/// 支持默认值、继承复制以及逐项设置/获取。
pub struct ResourceLimits {
    /// 最大打开文件描述符数量。
    pub max_fds: usize,
    /// 最大线程数。
    pub max_threads: usize,
    /// 最大栈大小（字节）。
    pub max_stack_size: usize,
    /// 最大数据段大小（字节）。
    pub max_data_size: usize,
    /// 最大文件大小（字节）。
    pub max_file_size: usize,
    /// 最大内存映射数量。
    pub max_mappings: usize,
    /// CPU 时间限制（秒）。
    pub cpu_time_limit: usize,
}

impl ResourceLimits {
    /// 返回默认的资源限制配置。
    pub fn default_limits() -> Self {
        eprintln!("[DBG] ResourceLimits::default_limits");
        Self {
            max_fds: 1024,
            max_threads: 256,
            max_stack_size: USR_STK_SZ * 4,
            max_data_size: KHEAP_SZ,
            max_file_size: usize::MAX,
            max_mappings: 65536,
            cpu_time_limit: 0,
        }
    }

    /// 检查当前文件描述符数量是否未超过上限。
    pub fn check_fd(&self, current: usize) -> bool {
        eprintln!("[DBG] ResourceLimits::check_fd");
        current < self.max_fds }
    /// 检查当前线程数是否未超过上限。
    pub fn check_threads(&self, current: usize) -> bool {
        eprintln!("[DBG] ResourceLimits::check_threads");
        current < self.max_threads }
    /// 检查请求的栈大小是否在限制范围内。
    pub fn check_stack(&self, requested: usize) -> bool {
        eprintln!("[DBG] ResourceLimits::check_stack");
        requested <= self.max_stack_size }
    /// 检查请求的数据段大小是否在限制范围内。
    pub fn check_data(&self, requested: usize) -> bool {
        eprintln!("[DBG] ResourceLimits::check_data");
        requested <= self.max_data_size }
    /// 检查请求的文件大小是否在限制范围内。
    pub fn check_filesize(&self, requested: usize) -> bool {
        eprintln!("[DBG] ResourceLimits::check_filesize");
        requested <= self.max_file_size }
    /// 检查当前内存映射数量是否未超过上限。
    pub fn check_mappings(&self, current: usize) -> bool {
        eprintln!("[DBG] ResourceLimits::check_mappings");
        current < self.max_mappings }

    /// 创建一个与当前限制完全相同的新实例（用于子进程继承）。
    pub fn inherit(&self) -> Self {
        eprintln!("[DBG] ResourceLimits::inherit");
        Self {
            max_fds: self.max_fds,
            max_threads: self.max_threads,
            max_stack_size: self.max_stack_size,
            max_data_size: self.max_data_size,
            max_file_size: self.max_file_size,
            max_mappings: self.max_mappings,
            cpu_time_limit: self.cpu_time_limit,
        }
    }

    /// 根据资源类型编号设置限制值。
    ///
    /// 资源编号：0=CPU时间, 1=文件大小, 2=数据段大小, 3=栈大小, 7=文件描述符数。
    pub fn set_limit(&mut self, resource: usize, value: usize) -> Result<(), &'static str> {
        eprintln!("[DBG] ResourceLimits::set_limit");
        match resource {
            0 => { self.cpu_time_limit = value; Ok(()) }
            1 => { self.max_file_size = value; Ok(()) }
            2 => { self.max_data_size = value; Ok(()) }
            3 => { self.max_stack_size = value; Ok(()) }
            7 => { self.max_fds = value; Ok(()) }
            _ => Err("einval"),
        }
    }

    /// 根据资源类型编号获取当前限制值。
    ///
    /// 资源编号：0=CPU时间, 1=文件大小, 2=数据段大小, 3=栈大小, 7=文件描述符数。
    pub fn get_limit(&self, resource: usize) -> Result<usize, &'static str> {
        eprintln!("[DBG] ResourceLimits::get_limit");
        match resource {
            0 => Ok(self.cpu_time_limit),
            1 => Ok(self.max_file_size),
            2 => Ok(self.max_data_size),
            3 => Ok(self.max_stack_size),
            7 => Ok(self.max_fds),
            _ => Err("einval"),
        }
    }

    /// 同时检查多项资源：文件描述符、线程数和栈大小。
    /// 只要有任意一项超出限制就返回 true。
    pub fn exceeds_any(&self, fds: usize, threads: usize, stack: usize) -> bool {
        eprintln!("[DBG] ResourceLimits::exceeds_any");
        let mut violations = 0usize;
        if fds > self.max_fds { violations += 1; }
        if threads > self.max_threads { violations += 1; }
        if stack > self.max_stack_size { violations += 1; }
        violations != 0
    }
}
