//! r8jit：r8 的 JIT（要 std，目前只有 x86-64 Unix）。
//!
//! 实现执行器的 [`Tier`]：普通 JS 函数每次进帧先到这里——
//!
//! ```text
//! 调用次数 / 循环圈数没到阈值 ⇒ 记一次，照常解释
//! 到阈值                     ⇒ r8compiler 编成机器码（编不了记下来，以后不再试）
//! 进帧时：整个函数都在机器码里 ⇒ 在本帧的寄存器与累加器上直接跑
//! 循环往回跳时：有这个循环头   ⇒ 从循环头切进机器码（OSR）
//!   返回                     ⇒ 结果在累加器，帧不用进了（OSR 的停在 return 前，解释器收尾）
//!   停在某条指令之前          ⇒ 快路前提不成立或机器码不收的指令，解释器从那里接着跑
//! ```
mod code;
mod helpers;

use core::cell::RefCell;

use r8virtasm::Function;
use r8compiler::Compiler;
use r8executor::{Tier, Tiered};
use r8frame::{FuncId, Value};

use code::Code;

/// 一个函数在 JIT 里的状态。
enum Slot {
    /// 还不热：进过几次、循环转过几圈
    Cold { calls: u32, loops: u32 },
    Ready(Ready),
    /// 编不了（后端拒收），以后不再试
    Failed,
}

/// 记过热度之后。
enum Heat<'s> {
    Cold,
    Ready(&'s Ready),
    Never,
}

/// 编好的函数。
struct Ready {
    code: Code,
    /// 整个函数都在机器码里：进帧时就能直接跑（否则只从循环头切进去）
    pure: bool,
    /// 能从中途进的循环头
    entries: Vec<u32>,
}

/// ⭐ JIT：交给执行器（`Executor::with_tier`）用。
pub struct Jit {
    /// 调用多少次算热
    threshold: u32,
    /// 按函数编号
    slots: RefCell<Vec<Slot>>,
    /// 第一次要编函数时才建
    compiler: core::cell::OnceCell<Compiler>,
}

impl Default for Jit {
    fn default() -> Self {
        Self::from(Jit::THRESHOLD)
    }
}

impl From<u32> for Jit {
    /// 调用 `threshold` 次（或循环转 [`LOOP_FACTOR`](Jit::LOOP_FACTOR) 倍那么多圈）之后编（0：第一次就编）。
    fn from(threshold: u32) -> Self {
        Self { threshold, slots: RefCell::new(Vec::new()), compiler: core::cell::OnceCell::new() }
    }
}

impl Jit {
    /// 缺省的热度阈值（调用次数）
    pub const THRESHOLD: u32 = 100;
    /// 循环圈数的阈值是调用次数阈值的这么多倍（一圈比一次调用轻）
    pub const LOOP_FACTOR: u32 = 10;

    /// 编过的函数个数与编不了的个数（诊断用）。
    pub fn counts(&self) -> (usize, usize) {
        let slots = self.slots.borrow();
        let ready = slots.iter().filter(|s| matches!(s, Slot::Ready(_))).count();
        let failed = slots.iter().filter(|s| matches!(s, Slot::Failed)).count();
        (ready, failed)
    }

    /// 编 `func`：成了是机器码，不成记成编不了。
    fn compile(&self, func: &Function) -> Slot {
        match self.compiler.get_or_init(Compiler::default).compile(func) {
            Ok(native) => match Code::load(&native) {
                Some(code) => Slot::Ready(Ready { code, pure: native.pure(), entries: native.entries }),
                None => Slot::Failed,
            },
            Err(_) => Slot::Failed,
        }
    }

    /// 记一次热度（`loop_turn`：循环转了一圈，否则是一次调用），够热就编。
    fn heat<'s>(&self, slots: &'s mut Vec<Slot>, id: FuncId, func: &Function, loop_turn: bool) -> Heat<'s> {
        let at = id.0 as usize;
        if at >= slots.len() {
            slots.resize_with(at + 1, || Slot::Cold { calls: 0, loops: 0 });
        }
        let slot = &mut slots[at];
        if let Slot::Cold { calls, loops } = slot {
            if loop_turn { *loops += 1 } else { *calls += 1 }
            if *calls <= self.threshold && *loops <= self.threshold.saturating_mul(Self::LOOP_FACTOR) {
                return Heat::Cold;
            }
            *slot = self.compile(func);
        }
        match slot {
            Slot::Ready(r) => Heat::Ready(r),
            Slot::Cold { .. } => Heat::Cold,
            Slot::Failed => Heat::Never,
        }
    }
}

impl Tier for Jit {
    fn enter(&self, id: FuncId, func: &Function, regs: &mut [Value<'_>], acc: &mut Value<'_>) -> Tiered {
        let mut slots = self.slots.borrow_mut();
        match self.heat(&mut slots, id, func, false) {
            Heat::Ready(r) if r.pure => Tiered::Ran(r.code.run(regs, acc, 0)),
            // 编好了但不能整体跑（只能从循环头切进去）、或者编不了：进帧时不用再问
            Heat::Ready(_) | Heat::Never => Tiered::Never,
            Heat::Cold => Tiered::NotYet,
        }
    }

    fn backedge(&self, id: FuncId, func: &Function, header: u32, regs: &mut [Value<'_>], acc: &mut Value<'_>) -> Tiered {
        let mut slots = self.slots.borrow_mut();
        match self.heat(&mut slots, id, func, true) {
            Heat::Ready(r) if r.entries.contains(&header) => Tiered::Ran(r.code.run(regs, acc, u64::from(header) + 1)),
            Heat::Ready(r) if r.entries.is_empty() => Tiered::Never,
            Heat::Ready(_) | Heat::Cold => Tiered::NotYet,
            Heat::Never => Tiered::Never,
        }
    }
}
