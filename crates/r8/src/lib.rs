//! r8 框架：引擎无关的抽象层。
//!
//! - [`Platform`]：线程与时钟，由嵌入方提供
//! - [`Isolate`]：一个独立的 JS 堆加句柄表加剖析器
//! - [`Context`]：一个全局对象下的执行环境
//! - [`HandleScope`] / [`Local`] / [`Global`]：栈上句柄与持久句柄
//! - [`Profiler`]：调用 / 回边计数
//! - [`Engine`]：各执行层（r8eval / mercedes）实现的统一入口
mod context;
mod engine;
mod error;
mod handle;
mod heap;
mod isolate;
mod ops;
mod platform;
mod profiler;
mod value;

pub use context::Context;
pub use engine::{Engine, Source, Tier};
pub use error::Error;
pub use handle::{Global, HandleScope, Local};
pub use heap::{Heap, ObjId, Object, StrId};
pub use isolate::{CreateParams, Isolate};
pub use ops::BinaryOp;
pub use platform::{DefaultPlatform, Platform, Task};
pub use profiler::{Counter, FuncId, Profiler};
pub use value::Value;
