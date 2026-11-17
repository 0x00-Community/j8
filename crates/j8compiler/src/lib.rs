//! j8compiler: bytecode function ⇒ raw machine code (`no_std`).
//!
//! ```text
//! j8virtasm::Function ─ Lower ─▶ MLIR (llvm dialect) ─ mlirpass X86_64Emit ─▶ x86-64 machine code + addresses to patch
//! ```
//!
//! The resulting machine code is a System V function `extern "C" fn(regs: *mut Value, acc: *mut Value, start: u64) ->
//! u64`: it runs directly on this frame's register file and accumulator (see [`ValueBits`](j8framework::ValueBits) for
//! the memory layout of [`Value`](j8framework::Value)); returning 0 means the function returned (result in the
//! accumulator), returning `pc + 1` means it stopped before instruction `pc` (j8executor's `Exit`).
//!
//! Instructions accepted by the first tier: register reads/writes, number and boolean constants, arithmetic and
//! comparisons, `++` / `--`, jumps, TDZ checks, return. If a runtime operand is not a number (or another fast-path
//! precondition fails), it exits before that instruction and the interpreter continues from there; other instructions
//! (calls, properties, closures, ...) also stop before themselves and are handed to the interpreter
//! ([`Native::exits`]).
//!
//! The third argument is where to enter: 0 at the function start, `h + 1` at loop header `h` ([`Native::entries`]:
//! switching in halfway through a running loop, i.e. OSR). Code entered midway through a loop also stops at `return`,
//! and the interpreter finishes the frame.
extern crate alloc;

mod helper;
mod lower;

pub use helper::Helper;
pub use lower::Unsupported;

use alloc::vec::Vec;

use mlirpass::mlirformat::MlirOpt;
use mlirpass::{Body, Ctx, Emit, Interfaces, PassManager, Passes, X86_64Emit};
use j8virtasm::Function;

/// A function's machine code: bytes and the absolute addresses (runtime functions) to patch.
#[derive(Clone, Debug)]
pub struct Native {
    pub bytes: Vec<u8>,
    /// (byte offset, whose address to patch): each is an 8-byte little-endian absolute address
    pub relocs: Vec<(usize, Helper)>,
    /// Instructions where machine code stops and hands off to the interpreter (not accepted by the first tier); if
    /// empty, the whole function is in machine code
    pub exits: Vec<Unsupported>,
    /// Loop headers that can be entered midway (OSR): passing `h + 1` as the third argument starts at instruction `h`
    pub entries: Vec<u32>,
}

impl Native {
    /// The whole function is in machine code (no instruction stops and hands off to the interpreter).
    pub fn pure(&self) -> bool {
        self.exits.is_empty()
    }
}

/// Compilation error.
#[derive(Clone, Debug)]
pub enum CompileError {
    /// Machine code can't help: the function cannot run as a whole in machine code (it has instructions to hand to the
    /// interpreter), and there is no loop to switch into
    Useless,
    /// Rejected by the backend (with the reason)
    Backend(alloc::string::String),
}

/// ⭐ The compiler: the MLIR dialect table and op interfaces are built once, then functions are compiled repeatedly.
pub struct Compiler {
    opt: MlirOpt,
    interfaces: Interfaces<'static>,
    /// Optimizations before emitting machine code: promoting locals to SSA (mem2reg), simplification, common
    /// subexpression elimination
    pipeline: PassManager,
}

impl Default for Compiler {
    /// Installs only what is needed: builtin (`module`) and the `llvm` dialect, plus `llvm`'s hand-written forms
    /// (`llvm.func` / `icmp` / `call` / …). Much lighter than `MlirOpt::upstream()` (built only when the JIT first
    /// compiles a function).
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
    /// Passes run before emitting machine code
    const PIPELINE: &str = "builtin.module(llvm.func(mem2reg, canonicalize, cse))";

    /// Bytecode function ⇒ machine code.
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
