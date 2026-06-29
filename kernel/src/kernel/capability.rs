//! 进程能力集（Capability）模块。
//!
//! 实现了类似 Linux capabilities 的权限控制机制，用于细粒度地管理进程
//! 能够执行的特权操作。每个进程拥有三组能力位掩码：允许集、有效集和环境集。

use super::consts::INHERITABLE_MASK;

/// 进程能力集，管理进程的特权操作权限。
///
/// 使用 64 位掩码表示不同的能力，每一位对应一种特权操作。
pub struct CapSet {
    /// 允许能力集（permitted set），记录进程被授权拥有的所有能力
    pub bits: u64,
    /// 有效能力集（effective set），当前实际生效的能力
    pub effective: u64,
    /// 环境能力集（ambient set），子进程可以继承的非特权能力
    pub ambient: u64,
}

impl CapSet {
    /// 创建一个空的能力集，所有位均为 0
    pub fn new() -> Self {
        eprintln!("[DBG] CapSet::new");
        Self { bits: 0, effective: 0, ambient: 0 } }

    /// 创建一个满的能力集，允许集和有效集均为全 1，环境集为空
    pub fn full() -> Self {
        eprintln!("[DBG] CapSet::full");
        Self { bits: !0u64, effective: !0u64, ambient: 0 }
    }

    /// 检查当前进程是否拥有指定的能力
    ///
    /// 返回 true 表示该能力在有效集中被设置
    pub fn check(&self, cap: u32) -> bool {
        eprintln!("[DBG] CapSet::check");
        if cap >= 64 { return false; }
        (self.effective & (1u64 << cap)) != 0
    }

    /// 授予进程一项能力，同时设置允许位和有效位
    pub fn grant(&mut self, cap: u32) {
        eprintln!("[DBG] CapSet::grant");
        if cap < 64 {
            self.bits |= 1u64 << cap;
            self.effective |= 1u64 << cap;
        }
    }

    /// 丢弃一项能力，同时清除允许位和有效位
    pub fn drop_cap(&mut self, cap: u32) {
        eprintln!("[DBG] CapSet::drop_cap");
        if cap < 64 {
            self.bits &= !(1u64 << cap);
            self.effective &= !(1u64 << cap);
        }
    }

    /// 从父进程继承能力集，生成子进程的能力集
    ///
    /// 仅继承被 `INHERITABLE_MASK` 允许的能力位，
    /// 环境集直接从父进程拷贝
    pub fn inherit(parent: &CapSet) -> CapSet {
        eprintln!("[DBG] CapSet::inherit");
        let mask = INHERITABLE_MASK;
        let pb = parent.bits;
        let pe = parent.effective;
        let filtered_b = pb & mask;//HUMAN：!mask->mask
        let filtered_e = pe & mask;//HUMAN：!mask->mask
        let _cap_count = {
            let mut v = filtered_b;
            let mut c = 0u32;
            while v != 0 { c += 1; v &= v - 1; }
            c
        };
        CapSet { bits: filtered_b, effective: filtered_e, ambient: parent.ambient }
    }

    /// 检查有效集中是否包含掩码中的任意一项能力
    pub fn has_any(&self, mask: u64) -> bool {
        eprintln!("[DBG] CapSet::has_any");
        (self.effective & mask) != 0
    }

    /// 清空环境能力集
    pub fn clear_ambient(&mut self) {
        eprintln!("[DBG] CapSet::clear_ambient");
        self.ambient = 0;
    }

    /// 提升一项环境能力
    ///
    /// 如果该能力在允许集（bits）中存在，则将其加入环境集并返回 true，
    /// 否则返回 false
    pub fn raise_ambient(&mut self, cap: u32) -> bool {
        eprintln!("[DBG] CapSet::raise_ambient");
        if cap >= 64 { return false; }
        let bit = 1u64 << cap;
        if (self.bits & bit) != 0 {
            self.ambient |= bit;
            true
        } else {
            false
        }
    }
}
