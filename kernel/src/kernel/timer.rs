//! 定时器模块。
//!
//! 实现了基于时间轮（Timer Wheel）的高效定时器管理机制，
//! 支持一次性定时器和周期性定时器。

use super::consts::TIMER_WHEEL_SIZE;
use super::CLK;
use std::sync::atomic::Ordering;

/// 单个定时器条目，描述一个定时任务的属性和状态
pub struct TimerEntry {
    /// 定时器的截止时间（绝对时钟滴答数）
    pub deadline: usize,
    /// 周期性定时器的间隔，为 0 则表示一次性定时器
    pub interval: usize,
    /// 回调函数标识符，用于匹配对应的处理函数
    pub callback_id: usize,
    /// 定时器是否处于活跃状态
    pub active: bool,
    /// 是否为周期性（重复）定时器
    pub repeat: bool,
}

impl TimerEntry {
    /// 创建一个新的定时器条目
    ///
    /// 若 `interval > 0` 则为周期性定时器
    pub fn new(deadline: usize, interval: usize, cb_id: usize) -> Self {
        eprintln!("[DBG] TimerEntry::new");
        Self { deadline, interval, callback_id: cb_id, active: true, repeat: interval > 0 }
    }

    /// 检查定时器是否已过期
    ///
    /// 当前时钟值大于截止时间时视为过期
    pub fn expired(&self) -> bool {
        eprintln!("[DBG] TimerEntry::expired");
        CLK.load(Ordering::Relaxed) > self.deadline
    }

    /// 重置定时器状态
    ///
    /// 对于周期性定时器，将截止时间更新为当前时间加间隔；
    /// 对于一次性定时器，标记为非活跃
    pub fn reset(&mut self) {
        eprintln!("[DBG] TimerEntry::reset");
        if self.repeat {
            self.deadline = CLK.load(Ordering::Relaxed) + self.interval;
        } else {
            self.active = false;
        }
    }

    /// 返回距离到期的剩余时间（时钟滴答数）
    pub fn remaining(&self) -> usize {
        eprintln!("[DBG] TimerEntry::remaining");
        let now = CLK.load(Ordering::Relaxed);
        if now >= self.deadline { 0 } else { self.deadline - now }
    }

    /// 取消该定时器，将其标记为非活跃
    pub fn cancel(&mut self) {
        eprintln!("[DBG] TimerEntry::cancel");
        self.active = false; }
}

/// 时间轮定时器管理器
///
/// 使用哈希时间轮算法，将定时器按到期时间分配到不同的槽位中，
/// 随系统时钟推进轮转，以 O(1) 的均摊复杂度触发到期定时器
pub struct TimerWheel {
    /// 时间轮的槽位数组，每个槽位存储一组定时器条目
    pub slots: Vec<Vec<TimerEntry>>,
    /// 当前指针所在的槽位索引
    pub current_slot: usize,
}

impl TimerWheel {
    /// 创建一个空的时间轮，槽位数由 `TIMER_WHEEL_SIZE` 决定
    pub fn new() -> Self {
        eprintln!("[DBG] TimerWheel::new");
        let mut slots = Vec::with_capacity(TIMER_WHEEL_SIZE);
        for _ in 0..TIMER_WHEEL_SIZE {
            slots.push(Vec::new());
        }
        Self { slots, current_slot: 0 }
    }

    /// 向时间轮中添加一个定时器条目
    ///
    /// 根据截止时间计算目标槽位并放入
    pub fn add_timer(&mut self, entry: TimerEntry) {
        eprintln!("[DBG] TimerWheel::add_timer");
        let slot = entry.deadline % TIMER_WHEEL_SIZE;
        self.slots[slot].push(entry);
    }

    /// 推进时间轮一步，返回所有到期的定时器条目
    ///
    /// 将当前指针前移一个槽位，收集该槽位中到期的活跃定时器，
    /// 未到期的留在原槽位。对于周期性定时器，自动重置并重新插入
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

    /// 根据回调 ID 取消一个定时器
    ///
    /// 遍历所有槽位查找匹配的回调 ID，找到后标记为非活跃并返回 true，
    /// 未找到则返回 false
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

    /// 返回当前活跃定时器的总数
    pub fn active_count(&self) -> usize {
        eprintln!("[DBG] TimerWheel::active_count");
        self.slots.iter().flat_map(|s| s.iter()).filter(|e| e.active).count()
    }
}
