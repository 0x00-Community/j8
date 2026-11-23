use alloc::vec::Vec;

use mlirpass::mlirformat::MlirOpt;
use mlirpass::{Body, Ctx, Interfaces, PassManager, Passes};

pub struct MachineCode {
    pub name: alloc::string::String,
    pub bytes: Vec<u8>,
    pub relocs: Vec<(usize, alloc::string::String)>,
}

pub trait Backend: Sync {
    fn emit(&self, body: &Body<'_>, root: mlirpass::OpId) -> Result<Vec<MachineCode>, mlirpass::mlirformat::Error>;
}

#[derive(Clone, Debug)]
pub enum CompileError {
    Backend(alloc::string::String),
}

pub struct Compiler {
    opt: MlirOpt,
    interfaces: Interfaces<'static>,
    pipeline: PassManager,
}

impl Default for Compiler {
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
    const PIPELINE: &str = "builtin.module(llvm.func(mem2reg, canonicalize, cse))";

    pub fn emit(&self, mlir: &str, backend: &dyn Backend) -> Result<Vec<MachineCode>, CompileError> {
        let failed = |e: mlirpass::mlirformat::Error| CompileError::Backend(e.0);
        let mut body = Body::parse(Ctx::new(&self.opt, &self.interfaces), mlir).map_err(failed)?;
        let root = body.root;
        self.pipeline.run_on(&mut body, root, &moremath::ThreadSingle).map_err(failed)?;
        backend.emit(&body, body.root).map_err(failed)
    }
}
