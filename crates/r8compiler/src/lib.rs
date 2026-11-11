//! r8compiler：字节码函数 ⇒ 原始机器码（`no_std`）。
//!
//! ```text
//! r8bytecode::Function ─ Lower ─▶ MLIR（llvm 方言）─ mlirpass X86_64Emit ─▶ x86-64 机器码 + 要填的地址
//! ```
//!
//! 出来的机器码是 System V 函数 `extern "C" fn(regs: *mut Value, acc: *mut Value) -> u64`：直接在本帧的寄存器文件
//! 与累加器上跑（[`Value`](r8frame::Value) 的内存布局见 [`ValueTag`](r8frame::ValueTag)），返回 0 表示函数返回了（结果在累加器），
//! 返回 `pc + 1` 表示停在第 `pc` 条指令之前（r8executor 的 `Exit`）。
//!
//! 第一档只收纯数值的函数：寄存器读写、数与布尔常量、四则运算与比较、`++` / `--`、跳转、死区检查、返回。
//! 运行时操作数不是数（或别的快路前提不成立）就停在那条指令之前退出，解释器从那里接着跑。
//! 别的指令（调用、属性、闭包……）整个函数不编（[`Unsupported`]）。
#![no_std]
extern crate alloc;

mod helper;
mod lower;

pub use helper::Helper;
pub use lower::Unsupported;

use alloc::vec::Vec;

use mlirpass::mlirformat::MlirOpt;
use mlirpass::{Body, Ctx, Emit, Interfaces, X86_64Emit};
use r8bytecode::Function;

/// 一个函数的机器码：字节与要填的绝对地址（运行时函数）。
#[derive(Clone, Debug)]
pub struct Native {
    pub bytes: Vec<u8>,
    /// （字节偏移，要填谁的地址）：每处是 8 字节小端绝对地址
    pub relocs: Vec<(usize, Helper)>,
}

/// 编译出错：函数里有第一档不收的指令，或者后端拒收（带原因）。
#[derive(Clone, Debug)]
pub enum CompileError {
    Unsupported(Unsupported),
    Backend(alloc::string::String),
}

impl TryFrom<&Function> for Native {
    type Error = CompileError;

    /// 字节码函数 ⇒ 机器码。
    fn try_from(func: &Function) -> Result<Self, CompileError> {
        let text = lower::Lower::module(func).map_err(CompileError::Unsupported)?;
        let opt = MlirOpt::upstream();
        let interfaces = Interfaces::upstream();
        let backend = |e: mlirpass::mlirformat::Error| CompileError::Backend(e.0);
        let body = Body::parse(Ctx::new(&opt, &interfaces), &text).map_err(backend)?;
        let codes = X86_64Emit.emit(&body, body.root).map_err(backend)?;
        let code = codes.into_iter().find(|c| c.name == lower::ENTRY).ok_or_else(|| CompileError::Backend("no entry function".into()))?;
        let relocs = code
            .relocs
            .iter()
            .map(|r| Helper::try_from(r.symbol.as_str()).map(|h| (r.offset, h)).map_err(|()| CompileError::Backend(alloc::format!("unknown symbol {}", r.symbol))))
            .collect::<Result<_, _>>()?;
        Ok(Native { bytes: code.bytes, relocs })
    }
}
