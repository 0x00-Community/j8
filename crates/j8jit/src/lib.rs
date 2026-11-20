//! j8jit: the JIT for j8 (needs std; currently x86-64 Unix only).
//!
//! j8 runs V8's Ignition bytecode; lowering it to MLIR is being rewritten for that bytecode. Until then [`Jit`] tells
//! the executor's [`Tier`] hook that no function gets machine code, and what remains is the pipeline after lowering:
//! [`Compiler`] (MLIR `llvm` dialect, optimized) and the x86-64 backend.
extern crate alloc;

mod compile;
mod helper;
mod x86_64;

pub use compile::{Backend, CompileError, Compiler, MachineCode};
pub use helper::Helper;
pub use x86_64::{X86_64Code, X86_64Emit, X86_64Reloc};

use j8bytecode::SharedFunction;
use j8executor::{Tier, Tiered};
use j8framework::{Agent, FuncId, Value};

/// ⭐ The JIT: handed to the executor (`Executor::with_tier`).
pub struct Jit {
    /// How many calls count as hot
    threshold: u32,
}

impl Default for Jit {
    fn default() -> Self {
        Self::from(Jit::THRESHOLD)
    }
}

impl From<u32> for Jit {
    /// Compiles after `threshold` calls (0: compile on the first one).
    fn from(threshold: u32) -> Self {
        Self { threshold }
    }
}

impl Jit {
    /// Default hotness threshold (call count)
    pub const THRESHOLD: u32 = 100;

    /// The hotness threshold
    pub fn threshold(&self) -> u32 {
        self.threshold
    }
}

impl Tier for Jit {
    fn enter(&self, _: &Agent<'_, '_>, _: FuncId, _: &SharedFunction, _: &mut [Value<'_>], _: &mut Value<'_>) -> Tiered {
        Tiered::Never
    }
}
