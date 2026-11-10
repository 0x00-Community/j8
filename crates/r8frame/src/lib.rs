//! r8frame：核心框架层，定义 trait 与通用结构体 / 枚举，具体实现在各层 crate（如 r8platform 实现 [`Platform`]）。
//!
//! 堆用 [gc-arena](https://docs.rs/gc-arena)：所有 JS 值带品牌生命周期 `'gc`，只能活在一次 [`Isolate::enter`] 里。
//!
//! - [`Platform`]：线程与时钟（r8platform 提供标准库实现）
//! - [`Isolate`]：一个独立的 JS 世界，持有 GC 堆与剖析器
//! - [`Scope`]：一次进入 isolate 的作用域；其中的 [`Value`] 就是 V8 的 Local，离开作用域即不可见
//! - [`Global`]：持久句柄，跨多次 `enter` 存活，drop 即放掉
//! - [`Context`]：一个全局对象下的执行环境
//! - [`Profiler`]：调用 / 回边计数
//! - [`Engine`]：执行引擎（r8executor）实现的统一入口
#![no_std]
extern crate alloc;

mod context;
mod engine;
mod error;
mod handle;
mod isolate;
mod object;
mod ops;
mod platform;
mod profiler;
mod value;

pub use context::Context;
pub use engine::{Engine, Source};
pub use error::Error;
pub use handle::Global;
pub use isolate::{CreateParams, Isolate, Scope};
pub use object::{Closure, Env, JsEnv, JsObject, JsString, ObjectData, ObjectKind};
pub use ops::{BinaryOp, CompareOp, ErrorKind, UnaryOp};
pub use platform::{Platform, Task};
pub use profiler::{Counter, FuncId, Profiler};
pub use value::Value;
