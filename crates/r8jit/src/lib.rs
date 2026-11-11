//! r8jit：r8 的 JIT（要 std，目前只有 x86-64 Unix）。
//!
//! 实现执行器的 [`Tier`]：普通 JS 函数每次进帧先到这里——
//!
//! ```text
//! 调用次数没到阈值        ⇒ 记一次，照常解释
//! 到阈值                  ⇒ r8compiler 编成机器码（编不了记下来，以后不再试）
//! 有机器码                ⇒ 在本帧的寄存器与累加器上直接跑
//!   返回                  ⇒ 结果在累加器，帧不用进了
//!   快路前提不成立中途退出 ⇒ 停在那条指令之前，解释器从那里接着跑
//! ```
mod code;
mod helpers;

use core::cell::RefCell;
use std::collections::BTreeMap;

use r8bytecode::Function;
use r8compiler::Native;
use r8executor::{Exit, Tier};
use r8frame::{FuncId, Value};

use code::Code;

/// 一个函数在 JIT 里的状态。
enum Slot {
    /// 还不热：进过几次
    Cold(u32),
    Ready(Code),
    /// 编不了（第一档不收的指令），以后不再试
    Failed,
}

/// ⭐ JIT：交给执行器（`Executor::with_tier`）用。
pub struct Jit {
    /// 调用多少次算热
    threshold: u32,
    slots: RefCell<BTreeMap<FuncId, Slot>>,
}

impl Default for Jit {
    fn default() -> Self {
        Self::from(Jit::THRESHOLD)
    }
}

impl From<u32> for Jit {
    /// 调用 `threshold` 次之后编（0：第一次就编）。
    fn from(threshold: u32) -> Self {
        Self { threshold, slots: RefCell::new(BTreeMap::new()) }
    }
}

impl Jit {
    /// 缺省的热度阈值
    pub const THRESHOLD: u32 = 100;

    /// 编过的函数个数与编不了的个数（诊断用）。
    pub fn counts(&self) -> (usize, usize) {
        let slots = self.slots.borrow();
        let ready = slots.values().filter(|s| matches!(s, Slot::Ready(_))).count();
        let failed = slots.values().filter(|s| matches!(s, Slot::Failed)).count();
        (ready, failed)
    }

    /// 编 `func`：成了是机器码，不成记成编不了。
    fn compile(func: &Function) -> Slot {
        match Native::try_from(func) {
            Ok(native) => Code::load(&native).map_or(Slot::Failed, Slot::Ready),
            Err(_) => Slot::Failed,
        }
    }
}

impl Tier for Jit {
    fn enter(&self, id: FuncId, func: &Function, regs: &mut [Value<'_>], acc: &mut Value<'_>) -> Option<Exit> {
        let mut slots = self.slots.borrow_mut();
        let slot = slots.entry(id).or_insert(Slot::Cold(0));
        if let Slot::Cold(n) = slot {
            *n += 1;
            if *n <= self.threshold {
                return None;
            }
            *slot = Self::compile(func);
        }
        match slot {
            Slot::Ready(code) => Some(code.run(regs, acc)),
            Slot::Cold(_) | Slot::Failed => None,
        }
    }
}
