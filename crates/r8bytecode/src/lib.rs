//! r8bytecode：累加器式寄存器字节码（r8executor 执行）。
//!
//! 字节码类型是 `no_std`；`compile` feature 带上 JS ⇒ 字节码编译器（`Module::try_from(&Source)`）。
#![cfg_attr(not(feature = "compile"), no_std)]
extern crate alloc;

#[cfg(feature = "compile")]
mod compiler;
mod insn;

pub use insn::{Constant, Function, Insn, Module, Reg};
