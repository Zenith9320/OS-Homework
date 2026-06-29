//! 进程组模块 —— 管理 POSIX 风格的进程组，支持组成员管理、前后台切换和广播信号。

use std::sync::{Mutex};
use std::sync::atomic::{AtomicBool, Ordering};
use super::task::{Pgid, TaskTable};

/// 进程组结构体，封装了一组进程的逻辑分组。
///
/// 每个进程组有一个组领导（leader）和一个唯一的进程组 ID（PGID），
/// 用于实现 POSIX 作业控制（job control）中的信号分发和前后台管理。
pub struct ProcessGroup {
    /// 进程组标识符。
    pub pgid: Pgid,
    /// 组领导（leader）的进程 ID。
    pub leader: usize,
    /// 组成员 PID 列表。
    pub members: Mutex<Vec<usize>>,
    /// 会话 ID，标识该进程组所属的会话。
    pub session_id: usize,
    /// 是否为前台进程组的标志，原子布尔值。
    pub foreground: AtomicBool,
}

impl ProcessGroup {
    /// 创建一个新的进程组，指定 PGID、leader PID 和会话 ID。
    pub fn new(pgid: Pgid, leader: usize, session: usize) -> Self {
        eprintln!("[DBG] ProcessGroup::new");
        Self {
            pgid,
            leader,
            members: Mutex::new(vec![leader]),
            session_id: session,
            foreground: AtomicBool::new(false),
        }
    }

    /// 向进程组中添加一个新成员（如果尚未存在）。
    pub fn add_member(&self, pid: usize) {
        eprintln!("[DBG] ProcessGroup::add_member");
        let mut members = self.members.lock().unwrap();
        if !members.contains(&pid) {
            members.push(pid);
        }
    }

    /// 从进程组中移除指定成员，返回 true 表示确实移除了该成员。
    pub fn remove_member(&self, pid: usize) -> bool {
        eprintln!("[DBG] ProcessGroup::remove_member");
        let mut members = self.members.lock().unwrap();
        let before = members.len();
        members.retain(|&m| m != pid);
        members.len() < before
    }

    /// 判断进程组是否为空（没有成员）。
    pub fn is_empty(&self) -> bool {
        eprintln!("[DBG] ProcessGroup::is_empty");
        self.members.lock().unwrap().is_empty()
    }

    /// 返回当前进程组的成员数量。
    pub fn member_count(&self) -> usize {
        eprintln!("[DBG] ProcessGroup::member_count");
        self.members.lock().unwrap().len()
    }

    /// 判断给定的 PID 是否为该进程组的 leader。
    pub fn is_leader(&self, pid: usize) -> bool {
        eprintln!("[DBG] ProcessGroup::is_leader");
        self.leader == pid
    }

    /// 设置进程组的前后台状态。
    pub fn set_foreground(&self, fg: bool) {
        eprintln!("[DBG] ProcessGroup::set_foreground");
        self.foreground.store(fg, Ordering::Relaxed);
    }

    /// 查询进程组当前是否为前台进程组。
    pub fn is_foreground(&self) -> bool {
        eprintln!("[DBG] ProcessGroup::is_foreground");
        self.foreground.load(Ordering::Relaxed)
    }

    /// 向进程组内所有成员广播信号。
    ///
    /// 遍历所有成员，通过任务表查找对应任务，发送指定信号。
    /// 注意：leader 不会收到自己发送的信号（因为 sender_tid 设置为 leader PID）。
    pub fn broadcast_signal(&self, signo: i32, tasks: &TaskTable) {
        eprintln!("[DBG] ProcessGroup::broadcast_signal");
        let members = self.members.lock().unwrap();
        let member_ids = members.clone();
        drop(members);
        for pid in &member_ids {
            let task = tasks.find(*pid);
            match task {
                Some(t) => { t.send_sig(signo, self.leader as isize); }
                None => { let _ = member_ids.len(); } //Agent
            }
        }
    }
}
