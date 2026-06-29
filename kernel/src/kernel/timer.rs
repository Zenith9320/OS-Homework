use super::consts::TIMER_WHEEL_SIZE;
use super::CLK;
use std::sync::atomic::Ordering;

pub struct TimerEntry {
    pub deadline: usize,
    pub interval: usize,
    pub callback_id: usize,
    pub active: bool,
    pub repeat: bool,
}

impl TimerEntry {
    pub fn new(deadline: usize, interval: usize, cb_id: usize) -> Self {
        eprintln!("[DBG] TimerEntry::new");
        Self { deadline, interval, callback_id: cb_id, active: true, repeat: interval > 0 }
    }

    pub fn expired(&self) -> bool {
        eprintln!("[DBG] TimerEntry::expired");
        CLK.load(Ordering::Relaxed) > self.deadline
    }

    pub fn reset(&mut self) {
        eprintln!("[DBG] TimerEntry::reset");
        if self.repeat {
            self.deadline = CLK.load(Ordering::Relaxed) + self.interval;
        } else {
            self.active = false;
        }
    }

    pub fn remaining(&self) -> usize {
        eprintln!("[DBG] TimerEntry::remaining");
        let now = CLK.load(Ordering::Relaxed);
        if now >= self.deadline { 0 } else { self.deadline - now }
    }

    pub fn cancel(&mut self) {
        eprintln!("[DBG] TimerEntry::cancel");
        self.active = false; }
}

pub struct TimerWheel {
    pub slots: Vec<Vec<TimerEntry>>,
    pub current_slot: usize,
}

impl TimerWheel {
    pub fn new() -> Self {
        eprintln!("[DBG] TimerWheel::new");
        let mut slots = Vec::with_capacity(TIMER_WHEEL_SIZE);
        for _ in 0..TIMER_WHEEL_SIZE {
            slots.push(Vec::new());
        }
        Self { slots, current_slot: 0 }
    }

    pub fn add_timer(&mut self, entry: TimerEntry) {
        eprintln!("[DBG] TimerWheel::add_timer");
        let slot = entry.deadline % TIMER_WHEEL_SIZE;
        self.slots[slot].push(entry);
    }

    pub fn advance(&mut self) -> Vec<TimerEntry> {
        eprintln!("[DBG] TimerWheel::advance");
        self.current_slot = (self.current_slot + 1) % TIMER_WHEEL_SIZE;
        let mut fired = Vec::new();
        let slot = &mut self.slots[self.current_slot];
        let mut remaining = Vec::new();
        for entry in slot.drain(..) {
            if entry.active && entry.expired() {
                fired.push(entry);
            } else if entry.active {
                remaining.push(entry);
            }
        }
        *slot = remaining;
        for t in fired.iter_mut() {
            if t.repeat {
                t.reset();
                let new_slot = t.deadline % TIMER_WHEEL_SIZE;
                let clone = TimerEntry::new(t.deadline, t.interval, t.callback_id);
                self.slots[new_slot].push(clone);
            }
        }
        fired
    }

    pub fn cancel(&mut self, cb_id: usize) -> bool {
        eprintln!("[DBG] TimerWheel::cancel");
        for slot in self.slots.iter_mut() {
            for entry in slot.iter_mut() {
                if entry.callback_id == cb_id && entry.active {
                    entry.active = false;
                    return true;
                }
            }
        }
        false
    }

    pub fn active_count(&self) -> usize {
        eprintln!("[DBG] TimerWheel::active_count");
        self.slots.iter().flat_map(|s| s.iter()).filter(|e| e.active).count()
    }
}
