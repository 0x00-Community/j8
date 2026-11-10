//! r8bytecode：JavaScript ⇒ 累加器式寄存器字节码（mercedes 执行）。
mod compiler;
mod insn;

pub use insn::{Constant, Function, Insn, Module, Reg};
