//! j8compiler: JavaScript ⇒ j8 bytecode, which is V8's (Ignition) byte for byte ([`j8bytecode::Module`]), parsing with
//! oxc. The code generator follows V8's `BytecodeGenerator` (`compiler.rs` / `expr.rs`) on top of V8's
//! `BytecodeArrayBuilder` (`builder.rs`).
//!
//! Every piece of JS goes through here before it runs: scripts, ES modules, and dynamic code (`eval`, `Function`).
//! There is no AST interpreter.
extern crate alloc;

mod builder;
mod compiler;
mod expr;
mod scope;

pub use compiler::Compile;
