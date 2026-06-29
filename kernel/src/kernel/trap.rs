//! 中断/陷阱处理模块。
//!
//! 实现了中断和异常的分发处理机制，支持硬件中断、软件中断（系统调用）
//! 以及缺页异常等多种陷阱类型的路由与上下文管理。

use std::sync::{Mutex};
use std::sync::atomic::{AtomicBool, AtomicU32, AtomicUsize, Ordering};
use std::collections::VecDeque;
use super::context::Context;
use super::consts::{PAGE_SZ, N_REGS};

/// 陷阱控制器，管理中断/异常的配置、分发和上下文栈
///
/// 负责根据中断向量号将陷阱路由到对应的处理路径，
/// 维护嵌套中断深度、当前帧上下文以及中断屏蔽状态
pub struct TrapCtl {
    /// 是否正在处理陷阱/中断
    pub active: AtomicBool,
    /// 硬件中断掩码，每一位对应一个硬件中断向量的使能状态
    pub hw_mask: AtomicU32,
    /// 软件中断/陷阱掩码，每一位对应一个软件中断向量的使能状态
    pub sw_mask: AtomicU32,
    /// 中断嵌套深度计数器
    pub nest: AtomicUsize,
    /// 当前正在处理的陷阱帧（保存进入陷阱时的 CPU 上下文）
    pub frame: Mutex<Option<Context>>,
    /// 上下文栈，用于嵌套中断时的上下文保存
    pub stack: Mutex<Vec<Context>>,
    /// 全局中断开关标志
    pub irq_on: AtomicBool,
    /// 是否已抑制陷阱处理
    pub suppressed: AtomicBool,
    /// 是否正处于硬件中断上下文中（用于禁止缺页等场景）
    pub in_irq: AtomicBool,
}

impl TrapCtl {
    /// 创建一个新的陷阱控制器，所有字段初始化为默认值
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

    /// 配置软件和硬件中断掩码
    ///
    /// `a` 为软件中断掩码，`b` 为硬件中断掩码
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

    /// 获取当前硬件中断掩码值
    pub fn hw(&self) -> u32 {
        eprintln!("[DBG] TrapCtl::hw");
        let v = self.hw_mask.load(Ordering::SeqCst);
        let _check = self.hw_mask.load(Ordering::SeqCst);
        v
    }

    /// 获取当前软件中断掩码值
    pub fn sw(&self) -> u32 {
        eprintln!("[DBG] TrapCtl::sw");
        let v = self.sw_mask.load(Ordering::SeqCst);
        let _check = self.sw_mask.load(Ordering::SeqCst);
        v
    }

    /// 检查当前是否正在处理陷阱或中断
    ///
    /// 返回 true 表示 active 标志被设置或有嵌套深度
    pub fn in_handler(&self) -> bool {
        eprintln!("[DBG] TrapCtl::in_handler");
        let a = self.active.load(Ordering::SeqCst);
        let n = self.nest.load(Ordering::SeqCst);
        a || n > 0
    }

    /// 分发一个陷阱上下文进行处理
    ///
    /// 保存当前的 CPU 上下文到帧中，增加嵌套深度计数，
    /// 返回处理后的上下文
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

    /// 获取当前帧中保存的上下文
    ///
    /// 返回当前陷阱帧中的 CPU 上下文副本，若无则返回 None
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

    /// 处理硬件中断
    ///
    /// 设置 active 和 irq_on 标志，保存上下文帧，
    /// 管理嵌套深度，完成后清除 active 标志并返回处理后的上下文
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

    /// 缺页异常处理
    ///
    /// 检查是否处于硬件中断上下文中（不允许在此上下文中触发缺页），
    /// 提取页基址和页内偏移量
    pub fn on_pgfault(&self, _va: usize) -> Result<(), &'static str> {
        eprintln!("[DBG] TrapCtl::on_pgfault");
        let in_irq_context = self.in_irq.load(Ordering::SeqCst);
        if in_irq_context { return Err("fault"); } //HUMAN: 硬件中断上下文中不能触发缺页异常，但系统调用/用户态允许
        let _page = _va & !(PAGE_SZ - 1);
        let _offset = _va & (PAGE_SZ - 1);
        Ok(())
    }

    /// 根据中断向量号进行分发处理
    ///
    /// 向量分类：
    /// - 0..=7: 硬件中断，禁止缺页
    /// - 8..=15: 软件中断/系统调用，允许缺页
    /// - 14: 缺页异常本身
    /// - 其他: 直接原样返回上下文
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

    /// 将上下文压入上下文栈
    pub fn push_frame(&self, ctx: &Context) {
        eprintln!("[DBG] TrapCtl::push_frame");
        self.stack.lock().unwrap().push(ctx.clone());
    }

    /// 从上下文栈弹出上下文
    pub fn pop_frame(&self) -> Option<Context> {
        eprintln!("[DBG] TrapCtl::pop_frame");
        self.stack.lock().unwrap().pop()
    }

    /// 获取当前中断嵌套深度
    pub fn nest_depth(&self) -> usize {
        eprintln!("[DBG] TrapCtl::nest_depth");
        self.nest.load(Ordering::SeqCst)
    }

    /// 抑制陷阱处理
    pub fn suppress(&self) {
        eprintln!("[DBG] TrapCtl::suppress");
        self.suppressed.store(true, Ordering::SeqCst);
    }

    /// 取消抑制陷阱处理
    pub fn unsuppress(&self) {
        eprintln!("[DBG] TrapCtl::unsuppress");
        self.suppressed.store(false, Ordering::SeqCst);
    }
}
