//! MLIR (llvm dialect) ⇒ raw machine code.
//!
//! ```text
//! V8 bytecode ─ (lowering: being rewritten for V8 bytecode) ─▶ MLIR (llvm dialect) ─ a `Backend` (j8jit's x86-64) ─▶
//! machine code + addresses to patch
//! ```
use alloc::vec::Vec;

use mlirpass::mlirformat::MlirOpt;
use mlirpass::{Body, Ctx, Interfaces, PassManager, Passes};

/// One function's machine code: its bytes, and where an absolute address of a symbol goes
pub struct MachineCode {
    pub name: alloc::string::String,
    pub bytes: Vec<u8>,
    /// (offset, symbol): eight bytes at `offset` take the symbol's address
    pub relocs: Vec<(usize, alloc::string::String)>,
}

/// What turns the lowered module (`llvm` dialect) into machine code, one per `llvm.func` (j8jit's x86-64)
pub trait Backend: Sync {
    fn emit(&self, body: &Body<'_>, root: mlirpass::OpId) -> Result<Vec<MachineCode>, mlirpass::mlirformat::Error>;
}

/// Compilation error.
#[derive(Clone, Debug)]
pub enum CompileError {
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

    /// A lowered module ⇒ one machine code per `llvm.func`, by `backend` (after the optimization pipeline).
    pub fn emit(&self, mlir: &str, backend: &dyn Backend) -> Result<Vec<MachineCode>, CompileError> {
        let failed = |e: mlirpass::mlirformat::Error| CompileError::Backend(e.0);
        let mut body = Body::parse(Ctx::new(&self.opt, &self.interfaces), mlir).map_err(failed)?;
        let root = body.root;
        self.pipeline.run(&mut body, root).map_err(failed)?;
        backend.emit(&body, body.root).map_err(failed)
    }
}
