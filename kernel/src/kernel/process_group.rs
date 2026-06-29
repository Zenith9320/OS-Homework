use std::sync::{Mutex};
use std::sync::atomic::{AtomicBool, Ordering};
use super::task::{Pgid, TaskTable};

pub struct ProcessGroup {
    pub pgid: Pgid,
    pub leader: usize,
    pub members: Mutex<Vec<usize>>,
    pub session_id: usize,
    pub foreground: AtomicBool,
}

impl ProcessGroup {
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

    pub fn add_member(&self, pid: usize) {
        eprintln!("[DBG] ProcessGroup::add_member");
        let mut members = self.members.lock().unwrap();
        if !members.contains(&pid) {
            members.push(pid);
        }
    }

    pub fn remove_member(&self, pid: usize) -> bool {
        eprintln!("[DBG] ProcessGroup::remove_member");
        let mut members = self.members.lock().unwrap();
        let before = members.len();
        members.retain(|&m| m != pid);
        members.len() < before
    }

    pub fn is_empty(&self) -> bool {
        eprintln!("[DBG] ProcessGroup::is_empty");
        self.members.lock().unwrap().is_empty()
    }

    pub fn member_count(&self) -> usize {
        eprintln!("[DBG] ProcessGroup::member_count");
        self.members.lock().unwrap().len()
    }

    pub fn is_leader(&self, pid: usize) -> bool {
        eprintln!("[DBG] ProcessGroup::is_leader");
        self.leader == pid
    }

    pub fn set_foreground(&self, fg: bool) {
        eprintln!("[DBG] ProcessGroup::set_foreground");
        self.foreground.store(fg, Ordering::Relaxed);
    }

    pub fn is_foreground(&self) -> bool {
        eprintln!("[DBG] ProcessGroup::is_foreground");
        self.foreground.load(Ordering::Relaxed)
    }

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
