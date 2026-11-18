//! j8compiler: JavaScript ⇒ j8 bytecode ([`j8bytecode::Module`]), parsing with oxc.
//!
//! Every piece of JS goes through here before it runs: scripts, ES modules, and dynamic code (`eval`, `Function`).
//! There is no AST interpreter.
//!
//! ```text
//! source ─ oxc parser + semantic checks ─▶ AST ─ scope resolution ─ code generation ─ peephole fusion ─▶ Module
//! ```
extern crate alloc;

mod compiler;
mod expr;
mod scope;

pub use compiler::Compile;
