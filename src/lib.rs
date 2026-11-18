//! j8: the facade for embedders. The binding macros and everything their generated code needs come from here:
//!
//! - `#[j8::class]`: exports an `impl` block as a JS class (constructor, prototype methods, accessors, static methods)
//! - `#[j8::object]`: exports an `impl` block of associated functions only as a plain object
//!   (like `console` / `process`)
//! - `#[j8::function]`: exports a function as a JS function
//! - `#[j8::module("node:fs", "fs")]`: exports an object of functions as a native module (`import` / `require`)
//! - `j8::runtime!`: declares a runtime (globals, native modules, prelude, optionally on top of another runtime);
//!   [`Runtime`] is a ready execution environment
//!
//! The macro reference is [USAGE.md](https://github.com/0x00-Community/j8/blob/main/crates/macros/USAGE.md); how to
//! put a whole host together is the guide below.
//!
//! The other j8 crates are re-exported by feature (each is also published on its own):
//!
//! | feature | module | |
//! |---|---|---|
//! | `runtime` (default) | `executor` + `builtins` + `platform` | everything needed to run JS |
//! | `bytecode` | [`bytecode`] | bytecode definitions (generated from JSON), assembler / disassembler |
//! | `compiler` | [`compiler`] | JS ⇒ bytecode |
//! | `executor` | [`executor`] | bytecode executor, implements `Engine` |
//! | `builtins` | [`builtins`] | built-in objects |
//! | `platform` | [`platform`] | the standard-library `Platform` |
//! | `taskqueue` | [`taskqueue`] | lock-free dataflow task queue |
//! | `lockfree` | [`lockfree`] | lock-free building blocks |
//! | `jit` | [`jit`] | bytecode ⇒ MLIR ⇒ x86-64 machine code + JIT scheduling (x86-64 Unix) |
//! | `json` | [`Json`] | Rust values crossing as JSON-shaped data through serde |
//! | `full` | all of the above | |
#![doc = include_str!("../EMBEDDING.md")]

#[cfg(feature = "json")]
pub use j8binding::Json;
pub use j8binding::{ArrayBuffer, Backing, BufferRef, External, Shared, SharedMemory, Transfer};
pub use j8binding::{Bytes, Class, ClassHandle, FromJs, Function, Install, IntoJs, Member, Module, Object, Persistent, Promise, Rest, This, NATIVE};
pub use j8macros::{class, function, module, object};

#[cfg(all(feature = "executor", feature = "builtins"))]
mod runtime;
#[cfg(all(feature = "executor", feature = "builtins"))]
pub use runtime::{Host, HostPlatform, Runtime};
#[cfg(all(feature = "executor", feature = "builtins"))]
pub use j8framework::{EventLoop, EventLoopSet};

pub use j8binding::Glue;
pub use j8framework as frame;

#[cfg(feature = "builtins")]
pub use j8builtins as builtins;
#[cfg(feature = "compiler")]
pub use j8compiler as compiler;
#[cfg(feature = "executor")]
pub use j8executor as executor;
#[cfg(feature = "jit")]
pub use j8jit as jit;
#[cfg(feature = "lockfree")]
pub use j8lockfree as lockfree;
#[cfg(feature = "napi")]
pub use j8napi as napi;
#[cfg(feature = "platform")]
pub use j8platform as platform;
#[cfg(feature = "taskqueue")]
pub use j8taskqueue as taskqueue;
#[cfg(feature = "bytecode")]
pub use j8bytecode as bytecode;
