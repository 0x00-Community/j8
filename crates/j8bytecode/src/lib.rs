//! j8bytecode: j8's virtual instruction set, an accumulator-based register bytecode (run by j8executor).
//!
//! Assembly / disassembly: `text.parse::<Module>()` / `module.to_string()`, inverses of each other.
//!
//! The `compile` feature (on by default) adds the JS ⇒ bytecode compiler
//! (`Module::try_from(&Source)`).
extern crate alloc;

#[cfg(feature = "compile")]
mod compiler;
#[cfg(feature = "compile")]
mod expr;
#[cfg(feature = "compile")]
mod scope;
mod asm;
mod disasm;
mod fuse;
mod insn;

pub use asm::AsmError;
pub use fuse::Unfused;
pub use insn::{Const, Constant, EvalName, EvalScope, Function, FunctionKind, Insn, Label, Mode, Module, PropertyKind, Reg};
