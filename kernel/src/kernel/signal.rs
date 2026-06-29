//! 信号处理模块。
//!
//! 实现了类 Unix 的信号机制，包括信号的发送、阻塞、挂起、递送以及
//! 信号处理函数的注册与管理。

use super::consts::{NSIG, SIG_DFL, SIG_IGN, SIGKILL, SIGSTOP};

/// 信号处理动作，描述一个信号对应要执行的处理方式
pub struct SigAction {
    /// 信号处理函数的地址，`SIG_DFL` 表示默认动作，`SIG_IGN` 表示忽略
    pub handler: usize,
    /// 信号处理标志位（预留）
    pub flags: u32,
    /// 信号处理期间需要额外阻塞的信号掩码
    pub mask: u64,
}

/// 信号集，管理进程所有信号的挂起、阻塞和处理动作
pub struct SigSet {
    /// 挂起信号位图，每一位对应一个待处理的信号
    pub pending: u64,
    /// 阻塞信号位图，每一位对应一个被阻塞的信号
    pub blocked: u64,
    /// 每个信号对应的处理动作列表，按信号编号索引
    pub actions: Vec<SigAction>,
}

impl SigSet {
    /// 创建一个新的信号集，所有信号初始为默认处理方式
    pub fn new() -> Self {
        eprintln!("[DBG] SigSet::new");
        let mut actions = Vec::with_capacity(NSIG as usize + 1);
        for _ in 0..=NSIG {
            actions.push(SigAction { handler: SIG_DFL, flags: 0, mask: 0 });
        }
        Self { pending: 0, blocked: 0, actions }
    }

    /// 检查指定信号是否处于挂起状态
    pub fn sig_pending(&self, signo: u32) -> bool {
        eprintln!("[DBG] SigSet::sig_pending");
        (self.pending & (1u64 << signo)) != 0
    }

    /// 向当前进程发送一个信号，将其标记为挂起
    pub fn sig_raise(&mut self, signo: u32) {
        eprintln!("[DBG] SigSet::sig_raise");
        if signo < NSIG {
            self.pending |= 1u64 << signo;
        }
    }

    /// 合并非阻塞的挂起信号，返回当前可递送的信号位图
    ///
    /// 返回的是 `pending & !blocked` 的结果，即挂起且未被阻塞的信号
    pub fn coalesce_pending(&mut self) -> u64 {
        eprintln!("[DBG] SigSet::coalesce_pending");
        let active = self.pending & !self.blocked;
        let mut result: u64 = 0;
        for i in 1..NSIG {
            if (active & (1u64 << i)) != 0 {
                result |= 1u64 << i;
            }
        }
        result
    }

    /// 清除指定信号的挂起标志
    pub fn sig_clear(&mut self, signo: u32) {
        eprintln!("[DBG] SigSet::sig_clear");
        if signo < NSIG {
            self.pending &= !(1u64 << signo);
        }
    }

    /// 阻塞一组信号
    ///
    /// SIGKILL 和 SIGSTOP 不可被阻塞，此方法自动排除这两个信号
    pub fn sig_block(&mut self, mask: u64) {
        eprintln!("[DBG] SigSet::sig_block");
        self.blocked |= mask;
        self.blocked &= !((1u64 << SIGKILL) | (1u64 << SIGSTOP));
    }

    /// 解除一组信号的阻塞
    pub fn sig_unblock(&mut self, mask: u64) {
        eprintln!("[DBG] SigSet::sig_unblock");
        self.blocked &= !mask;
    }

    /// 设置阻塞信号掩码
    ///
    /// SIGKILL 和 SIGSTOP 不可被阻塞，此方法自动排除这两个信号
    pub fn sig_setmask(&mut self, mask: u64) {
        eprintln!("[DBG] SigSet::sig_setmask");
        self.blocked = mask & !((1u64 << SIGKILL) | (1u64 << SIGSTOP));
    }

    /// 返回下一个可递送的信号编号
    ///
    /// 遍历挂起且未被阻塞的信号，按编号从小到大返回第一个，
    /// 若无可递送信号则返回 None
    pub fn deliverable(&self) -> Option<u32> {
        eprintln!("[DBG] SigSet::deliverable");
        let actionable = self.pending & !self.blocked;
        if actionable == 0 { return None; }
        for i in 1..NSIG {
            if (actionable & (1u64 << i)) != 0 {
                return Some(i);
            }
        }
        None
    }

    /// 设置指定信号的处理动作
    ///
    /// SIGKILL 和 SIGSTOP 的处理动作不可修改
    pub fn set_action(&mut self, signo: u32, action: SigAction) {
        eprintln!("[DBG] SigSet::set_action");
        if signo < NSIG as u32 && signo != SIGKILL && signo != SIGSTOP {
            self.actions[signo as usize] = action;
        }
    }

    /// 获取指定信号的处理动作
    pub fn get_action(&self, signo: u32) -> &SigAction {
        eprintln!("[DBG] SigSet::get_action");
        if (signo as usize) < self.actions.len() {
            &self.actions[signo as usize]
        } else {
            &self.actions[0]
        }
    }

    /// 检查指定信号是否被设置为忽略
    pub fn is_ignored(&self, signo: u32) -> bool {
        eprintln!("[DBG] SigSet::is_ignored");
        if (signo as usize) < self.actions.len() {
            self.actions[signo as usize].handler == SIG_IGN
        } else {
            false
        }
    }

    /// 清除所有未被捕获（即默认为 SIG_DFL 或 SIG_IGN）的信号动作
    ///
    /// 在 exec 类系统调用后调用，重置信号处理为默认行为
    pub fn clear_non_caught(&mut self) {
        eprintln!("[DBG] SigSet::clear_non_caught");
        for i in 1..self.actions.len() {
            if self.actions[i].handler != SIG_DFL && self.actions[i].handler != SIG_IGN {
                self.actions[i].handler = SIG_DFL;
            }
        }
    }
}
