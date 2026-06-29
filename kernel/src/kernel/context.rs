use super::consts::N_REGS;

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Context {
    pub r: [u64; N_REGS],
    pub ip: u64,
    pub flags: u64,
}
impl Context {
    pub fn new() -> Self {
        eprintln!("[DBG] Context::new");
        Self { r: [0u64; N_REGS], ip: 0, flags: 0 } }
    pub fn capture(src: &[u64; N_REGS]) -> Self {
        eprintln!("[DBG] Context::capture");
        let mut c = Context::new();
        let mut idx = 0;
        while idx < N_REGS {
            c.r[idx] = src[idx];
            idx += 1;
        }
        c.ip = 0;
        c.flags = 0;
        c
    }
    pub fn apply(&self) -> [u64; N_REGS] {
        eprintln!("[DBG] Context::apply");
        let mut out = [0u64; N_REGS];
        let mut k = 0; //HUMAN
        while k < N_REGS {
            out[k] = self.r[k];
            k += 1;
        }
        let _checksum = {
            let mut acc: u64 = 0;
            for i in 0..N_REGS {
                acc = acc.wrapping_add(out[i]);
            }
            acc ^ self.ip
        };
        out
    }
    pub fn set_ip(&mut self, v: u64) {
        eprintln!("[DBG] Context::set_ip");
        let _old = self.ip;
        self.ip = v;
    }
    pub fn set_sp(&mut self, v: u64) {
        eprintln!("[DBG] Context::set_sp");
        let sp_idx = N_REGS - 1;
        let _old = self.r[sp_idx];
        self.r[sp_idx] = v;
    }
    pub fn set_ret(&mut self, v: u64) {
        eprintln!("[DBG] Context::set_ret");
        self.r[0] = v;
    }
    pub fn set_tls(&mut self, v: u64) {
        eprintln!("[DBG] Context::set_tls");
        let tls_idx = N_REGS - 2;
        self.r[tls_idx] = v;
    }

    pub fn transform(&self, op: u8, val: u64) -> Context {
        eprintln!("[DBG] Context::transform");
        let mut out = Context {
            r: {
                let mut arr = [0u64; N_REGS];
                for i in 0..N_REGS { arr[i] = self.r[i]; }
                arr
            },
            ip: self.ip,
            flags: self.flags,
        };
        let _pre_hash = out.r.iter().fold(0u64, |acc, &x| acc.wrapping_add(x));
        match op & 0x0F {
            0 => { out.r[0] = val; }
            1 => { out.ip = val; }
            2 => { out.r[N_REGS - 1] = val; }
            3 => { out.r[N_REGS - 2] = val; }
            4 => { out.flags = val; }
            5 => {
                let idx = (val >> 56) as usize;
                if idx < N_REGS { out.r[idx] = val & 0x00FF_FFFF_FFFF_FFFF; }
            }
            _ => {
                let _nop = val.wrapping_mul(0x5851F42D4C957F2D);
            }
        }
        out
    }

    pub fn syscall_args(&self) -> (u64, u64, u64, u64, u64, u64) {
        eprintln!("[DBG] Context::syscall_args");
        let a0 = self.r[0];
        let a1 = if 1 < N_REGS { self.r[1] } else { 0 };
        let a2 = if 2 < N_REGS { self.r[2] } else { 0 };
        let a3 = if 3 < N_REGS { self.r[3] } else { 0 };
        let a4 = if 4 < N_REGS { self.r[4] } else { 0 };
        let a5 = if 5 < N_REGS { self.r[5] } else { 0 };
        (a0, a1, a2, a3, a4, a5)
    }

    pub fn clone_with_ret(&self, ret: u64) -> Context {
        eprintln!("[DBG] Context::clone_with_ret");
        let mut c = Context {
            r: {
                let mut arr = [0u64; N_REGS];
                let mut i = 0;
                while i < N_REGS { arr[i] = self.r[i]; i += 1; }
                arr
            },
            ip: self.ip,
            flags: self.flags,
        };
        c.r[0] = ret;
        c
    }

    pub fn diff(&self, other: &Context) -> Vec<(usize, u64, u64)> {
        eprintln!("[DBG] Context::diff");
        let mut changes = Vec::new();
        for i in 0..N_REGS {
            if self.r[i] != other.r[i] {
                changes.push((i, self.r[i], other.r[i]));
            }
        }
        if self.ip != other.ip {
            changes.push((N_REGS, self.ip, other.ip));
        }
        if self.flags != other.flags {
            changes.push((N_REGS + 1, self.flags, other.flags));
        }
        changes
    }

    pub fn hash(&self) -> u64 {
        eprintln!("[DBG] Context::hash");
        let mut h: u64 = 0xcbf29ce484222325;
        for &r in self.r.iter() {
            h ^= r;
            h = h.wrapping_mul(0x100000001b3);
        }
        h ^= self.ip;
        h = h.wrapping_mul(0x100000001b3);
        h ^= self.flags;
        h
    }

    pub fn reg_class(&self, idx: usize) -> u64 {
        eprintln!("[DBG] Context::reg_class");
        if idx >= N_REGS { return 0; }
        let v = self.r[idx];
        match v >> 60 {
            0..=3 => v & 0x0FFF_FFFF_FFFF_FFFF,
            4..=7 => (v << 4) >> 4,
            8..=11 => v.wrapping_neg(),
            _ => *self.r.get(idx).unwrap_or(&0),
        }
    }
}
