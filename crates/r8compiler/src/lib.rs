//! r8compiler：字节码函数 ⇒ 原始机器码（`no_std`）。
//!
//! ```text
//! r8virtasm::Function ─ Lower ─▶ MLIR（llvm 方言）─ mlirpass X86_64Emit ─▶ x86-64 机器码 + 要填的地址
//! ```
//!
//! 出来的机器码是 System V 函数 `extern "C" fn(regs: *mut Value, acc: *mut Value, start: u64) -> u64`：直接在本帧的寄存器文件
//! 与累加器上跑（[`Value`](r8framework::Value) 的内存布局见 [`ValueTag`](r8framework::ValueTag)），返回 0 表示函数返回了（结果在累加器），
//! 返回 `pc + 1` 表示停在第 `pc` 条指令之前（r8executor 的 `Exit`）。
//!
//! 第一档收的指令：寄存器读写、数与布尔常量、四则运算与比较、`++` / `--`、跳转、死区检查、返回。
//! 运行时操作数不是数（或别的快路前提不成立）就停在那条指令之前退出，解释器从那里接着跑；
//! 别的指令（调用、属性、闭包……）也是停在它之前交给解释器（[`Native::exits`]）。
//!
//! 第三个参数是从哪进：0 从函数开头，`h + 1` 从循环头 `h`（[`Native::entries`]，循环跑到一半切进来，OSR）。
//! 从循环中途进的遇到 `return` 也停下，帧由解释器收尾。
#![no_std]
extern crate alloc;

mod helper;
mod lower;

pub use helper::Helper;
pub use lower::Unsupported;

use alloc::vec::Vec;

use mlirpass::mlirformat::MlirOpt;
use mlirpass::{Body, Ctx, Emit, Interfaces, PassManager, Passes, X86_64Emit};
use r8virtasm::Function;

/// 一个函数的机器码：字节与要填的绝对地址（运行时函数）。
#[derive(Clone, Debug)]
pub struct Native {
    pub bytes: Vec<u8>,
    /// （字节偏移，要填谁的地址）：每处是 8 字节小端绝对地址
    pub relocs: Vec<(usize, Helper)>,
    /// 机器码停下交给解释器执行的指令（第一档不收的）；空的话整个函数都在机器码里
    pub exits: Vec<Unsupported>,
    /// 能从中途进的循环头（OSR）：第三个参数传 `h + 1` 就从第 `h` 条指令开始
    pub entries: Vec<u32>,
}

impl Native {
    /// 整个函数都在机器码里（没有停下交给解释器的指令）。
    pub fn pure(&self) -> bool {
        self.exits.is_empty()
    }
}

/// 编译出错。
#[derive(Clone, Debug)]
pub enum CompileError {
    /// 机器码帮不上忙：函数不能整体在机器码里跑（有要交给解释器的指令），也没有能切进去的循环
    Useless,
    /// 后端拒收（带原因）
    Backend(alloc::string::String),
}

/// ⭐ 编译器：MLIR 的方言表与 op 接口只建一次，反复编函数。
pub struct Compiler {
    opt: MlirOpt,
    interfaces: Interfaces<'static>,
    /// 出机器码之前的优化：局部变量提成 SSA（mem2reg）、化简、公共子表达式
    pipeline: PassManager,
}

impl Default for Compiler {
    /// 只装要用的：builtin（`module`）与 `llvm` 方言、`llvm` 的手写形式（`llvm.func` / `icmp` / `call` / …）。
    /// 比 `MlirOpt::upstream()` 轻得多（JIT 第一次编函数时才建）。
    fn default() -> Self {
        let mut opt = MlirOpt::new();
        opt.load(mlirpass::mlirformat::builtin::BuiltinDialect).load(mlirpass::mlirformat::llvm::LlvmDialect);
        for t in mlirpass::mlirformat::llvm_hand::LlvmHand::templates() {
            opt.inject(t);
        }
        let passes = Passes::upstream();
        let mut pipeline = PassManager::parse(&passes, Self::PIPELINE).expect("the pipeline names passes mlirpass always has");
        pipeline.verify_each = false;
        Self { opt, interfaces: Interfaces::upstream(), pipeline }
    }
}

impl Compiler {
    /// 出机器码之前跑的 pass
    const PIPELINE: &str = "builtin.module(llvm.func(mem2reg, canonicalize, cse))";

    /// 字节码函数 ⇒ 机器码。
    pub fn compile(&self, func: &Function) -> Result<Native, CompileError> {
        let lowered = lower::Lower::module(func);
        if !lowered.exits.is_empty() && lowered.headers.is_empty() {
            return Err(CompileError::Useless);
        }
        let backend = |e: mlirpass::mlirformat::Error| CompileError::Backend(e.0);
        let mut body = Body::parse(Ctx::new(&self.opt, &self.interfaces), &lowered.text).map_err(backend)?;
        let root = body.root;
        self.pipeline.run(&mut body, root).map_err(backend)?;
        let codes = X86_64Emit.emit(&body, body.root).map_err(backend)?;
        let code = codes.into_iter().find(|c| c.name == lower::ENTRY).ok_or_else(|| CompileError::Backend("no entry function".into()))?;
        let relocs = code
            .relocs
            .iter()
            .map(|r| Helper::try_from(r.symbol.as_str()).map(|h| (r.offset, h)).map_err(|()| CompileError::Backend(alloc::format!("unknown symbol {}", r.symbol))))
            .collect::<Result<_, _>>()?;
        Ok(Native { bytes: code.bytes, relocs, exits: lowered.exits, entries: lowered.headers })
    }
}
