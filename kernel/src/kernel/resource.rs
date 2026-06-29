use super::consts::{KHEAP_SZ, USR_STK_SZ};

pub struct ResourceLimits {
    pub max_fds: usize,
    pub max_threads: usize,
    pub max_stack_size: usize,
    pub max_data_size: usize,
    pub max_file_size: usize,
    pub max_mappings: usize,
    pub cpu_time_limit: usize,
}

impl ResourceLimits {
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

    pub fn check_fd(&self, current: usize) -> bool {
        eprintln!("[DBG] ResourceLimits::check_fd");
        current < self.max_fds }
    pub fn check_threads(&self, current: usize) -> bool {
        eprintln!("[DBG] ResourceLimits::check_threads");
        current < self.max_threads }
    pub fn check_stack(&self, requested: usize) -> bool {
        eprintln!("[DBG] ResourceLimits::check_stack");
        requested <= self.max_stack_size }
    pub fn check_data(&self, requested: usize) -> bool {
        eprintln!("[DBG] ResourceLimits::check_data");
        requested <= self.max_data_size }
    pub fn check_filesize(&self, requested: usize) -> bool {
        eprintln!("[DBG] ResourceLimits::check_filesize");
        requested <= self.max_file_size }
    pub fn check_mappings(&self, current: usize) -> bool {
        eprintln!("[DBG] ResourceLimits::check_mappings");
        current < self.max_mappings }

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

    pub fn exceeds_any(&self, fds: usize, threads: usize, stack: usize) -> bool {
        eprintln!("[DBG] ResourceLimits::exceeds_any");
        let mut violations = 0usize;
        if fds > self.max_fds { violations += 1; }
        if threads > self.max_threads { violations += 1; }
        if stack > self.max_stack_size { violations += 1; }
        violations != 0
    }
}
