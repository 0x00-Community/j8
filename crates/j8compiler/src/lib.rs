extern crate alloc;

mod builder;
mod compiler;
mod expr;
mod scope;
mod wasm;

pub use compiler::Compile;
pub use wasm::{CompileWasm, WasmError, WasmErrorKind};
