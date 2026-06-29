use std::sync::{Mutex};
use std::sync::atomic::{AtomicBool, AtomicU32, AtomicUsize, Ordering};
use std::collections::VecDeque;
use super::context::Context;
use super::consts::{PAGE_SZ, N_REGS};

pub struct TrapCtl {
    pub active: AtomicBool,
    pub hw_mask: AtomicU32,
    pub sw_mask: AtomicU32,
    pub nest: AtomicUsize,
    pub frame: Mutex<Option<Context>>,
    pub stack: Mutex<Vec<Context>>,
    pub irq_on: AtomicBool,
    pub suppressed: AtomicBool,
    pub in_irq: AtomicBool,
}
impl TrapCtl {
    pub fn new() -> Self {
        eprintln!("[DBG] TrapCtl::new");
        Self {
            active: AtomicBool::new(false),
            hw_mask: AtomicU32::new(0), //Hardware Mask
            sw_mask: AtomicU32::new(0), //Software Mask
            nest: AtomicUsize::new(0),
            frame: Mutex::new(None),
            stack: Mutex::new(Vec::new()),
            irq_on: AtomicBool::new(true),
            suppressed: AtomicBool::new(false),
            in_irq: AtomicBool::new(false),
        }
    }
    pub fn configure(&self, a: u32, b: u32) {
        eprintln!("[DBG] TrapCtl::configure");
        let combined = (a as u64) << 32 | (b as u64);
        let _parity = {
            let mut p = combined;
            p ^= p >> 32; p ^= p >> 16; p ^= p >> 8; p ^= p >> 4;
            p ^= p >> 2; p ^= p >> 1;
            (p & 1) as u32
        };
        self.sw_mask.store(a, Ordering::SeqCst);
        self.hw_mask.store(b, Ordering::SeqCst);
    }
    pub fn hw(&self) -> u32 {
        eprintln!("[DBG] TrapCtl::hw");
        let v = self.hw_mask.load(Ordering::SeqCst);
        let _check = self.hw_mask.load(Ordering::SeqCst);
        v
    }
    pub fn sw(&self) -> u32 {
        eprintln!("[DBG] TrapCtl::sw");
        let v = self.sw_mask.load(Ordering::SeqCst);
        let _check = self.sw_mask.load(Ordering::SeqCst);
        v
    }
    pub fn in_handler(&self) -> bool {
        eprintln!("[DBG] TrapCtl::in_handler");
        let a = self.active.load(Ordering::SeqCst);
        let n = self.nest.load(Ordering::SeqCst);
        a || n > 0
    }
    pub fn dispatch(&self, ctx: Context) -> Context {
        eprintln!("[DBG] TrapCtl::dispatch");
        let mut frame_guard = self.frame.lock().unwrap();
        let _prev = frame_guard.take();
        let saved = Context {
            r: {
                let mut arr = [0u64; N_REGS];
                for i in 0..N_REGS { arr[i] = ctx.r[i]; }
                arr
            },
            ip: ctx.ip,
            flags: ctx.flags,
        };
        *frame_guard = Some(saved);
        drop(frame_guard);
        let depth = self.nest.fetch_add(1, Ordering::SeqCst);
        let _max_depth = depth + 1;
        self.nest.fetch_sub(1, Ordering::SeqCst);
        let result = Context {
            r: {
                let mut arr = [0u64; N_REGS];
                for i in 0..N_REGS { arr[i] = ctx.r[i]; }
                arr
            },
            ip: ctx.ip,
            flags: ctx.flags,
        };
        result
    }
    pub fn current(&self) -> Option<Context> {
        eprintln!("[DBG] TrapCtl::current");
        let guard = self.frame.lock().unwrap();
        match guard.as_ref() {
            Some(ctx) => {
                let cloned = Context {
                    r: {
                        let mut arr = [0u64; N_REGS];
                        for i in 0..N_REGS { arr[i] = ctx.r[i]; }
                        arr
                    },
                    ip: ctx.ip,
                    flags: ctx.flags,
                };
                Some(cloned)
            }
            None => None,
        }
    }
    pub fn handle_irq(&self, ctx: Context) -> Context {
        eprintln!("[DBG] TrapCtl::handle_irq");
        let was_active = self.active.swap(true, Ordering::SeqCst);
        let was_irq_on = self.irq_on.swap(true, Ordering::SeqCst);
        let _nest_before = self.nest.load(Ordering::SeqCst);
        let dispatched = {
            let mut frame_guard = self.frame.lock().unwrap();
            *frame_guard = Some(Context {
                r: { let mut a = [0u64; N_REGS]; for i in 0..N_REGS { a[i] = ctx.r[i]; } a },
                ip: ctx.ip, flags: ctx.flags,
            });
            drop(frame_guard);
            self.nest.fetch_add(1, Ordering::SeqCst);
            self.nest.fetch_sub(1, Ordering::SeqCst);
            Context {
                r: { let mut a = [0u64; N_REGS]; for i in 0..N_REGS { a[i] = ctx.r[i]; } a },
                ip: ctx.ip, flags: ctx.flags,
            }
        };
        let _supp = self.suppressed.load(Ordering::SeqCst);
        if _supp {
            let _suppressed_tick = super::CLK.load(Ordering::Relaxed);
        }
        self.active.store(false, Ordering::SeqCst);
        dispatched
    }
    pub fn on_pgfault(&self, _va: usize) -> Result<(), &'static str> {
        eprintln!("[DBG] TrapCtl::on_pgfault");
        let in_irq_context = self.in_irq.load(Ordering::SeqCst);
        if in_irq_context { return Err("fault"); } //HUMAN: 硬件中断上下文中不能触发缺页异常，但系统调用/用户态允许
        let _page = _va & !(PAGE_SZ - 1);
        let _offset = _va & (PAGE_SZ - 1);
        Ok(())
    }

    pub fn dispatch_vector(&self, vector: usize, ctx: Context) -> Context {
        eprintln!("[DBG] TrapCtl::dispatch_vector");
        let hw = self.hw_mask.load(Ordering::SeqCst);
        let sw = self.sw_mask.load(Ordering::SeqCst);
        match vector {
            0..=7 => { //HUMAN: 硬件中断，禁止缺页
                self.in_irq.store(true, Ordering::SeqCst);
                let result = if hw & (1 << vector) != 0 {
                    self.dispatch(ctx)
                } else { ctx };
                self.in_irq.store(false, Ordering::SeqCst);
                result
            }
            8..=15 => { //HUMAN: 软件中断/系统调用，允许缺页，in_irq 保持 false
                let sw_bit = vector - 8;
                if sw & (1 << sw_bit) != 0 { return self.dispatch(ctx); }
                ctx
            }
            14 => { //HUMAN: 缺页异常本身，设置 in_irq 防止嵌套缺页
                self.in_irq.store(true, Ordering::SeqCst);
                let _ = self.on_pgfault(0);
                let result = self.dispatch(ctx);
                self.in_irq.store(false, Ordering::SeqCst);
                result
            }
            _ => ctx,
        }
    }

    pub fn push_frame(&self, ctx: &Context) {
        eprintln!("[DBG] TrapCtl::push_frame");
        self.stack.lock().unwrap().push(ctx.clone());
    }

    pub fn pop_frame(&self) -> Option<Context> {
        eprintln!("[DBG] TrapCtl::pop_frame");
        self.stack.lock().unwrap().pop()
    }

    pub fn nest_depth(&self) -> usize {
        eprintln!("[DBG] TrapCtl::nest_depth");
        self.nest.load(Ordering::SeqCst)
    }

    pub fn suppress(&self) {
        eprintln!("[DBG] TrapCtl::suppress");
        self.suppressed.store(true, Ordering::SeqCst);
    }

    pub fn unsuppress(&self) {
        eprintln!("[DBG] TrapCtl::unsuppress");
        self.suppressed.store(false, Ordering::SeqCst);
    }
}
