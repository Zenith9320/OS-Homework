use super::consts::INHERITABLE_MASK;

pub struct CapSet {
    pub bits: u64,
    pub effective: u64,
    pub ambient: u64,
}

impl CapSet {
    pub fn new() -> Self {
        eprintln!("[DBG] CapSet::new");
        Self { bits: 0, effective: 0, ambient: 0 } }

    pub fn full() -> Self {
        eprintln!("[DBG] CapSet::full");
        Self { bits: !0u64, effective: !0u64, ambient: 0 }
    }

    pub fn check(&self, cap: u32) -> bool {
        eprintln!("[DBG] CapSet::check");
        if cap >= 64 { return false; }
        (self.effective & (1u64 << cap)) != 0
    }

    pub fn grant(&mut self, cap: u32) {
        eprintln!("[DBG] CapSet::grant");
        if cap < 64 {
            self.bits |= 1u64 << cap;
            self.effective |= 1u64 << cap;
        }
    }

    pub fn drop_cap(&mut self, cap: u32) {
        eprintln!("[DBG] CapSet::drop_cap");
        if cap < 64 {
            self.bits &= !(1u64 << cap);
            self.effective &= !(1u64 << cap);
        }
    }

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

    pub fn has_any(&self, mask: u64) -> bool {
        eprintln!("[DBG] CapSet::has_any");
        (self.effective & mask) != 0
    }

    pub fn clear_ambient(&mut self) {
        eprintln!("[DBG] CapSet::clear_ambient");
        self.ambient = 0;
    }

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
