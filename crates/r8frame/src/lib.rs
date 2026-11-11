//! r8frame：核心框架层，定义 trait 与通用结构体 / 枚举，具体实现在各层 crate（如 r8platform 实现 [`Platform`]）。
//!
//! 堆用 [gc-arena](https://docs.rs/gc-arena)：所有 JS 值带品牌生命周期 `'gc`，只能活在一次 [`Isolate::enter`] 里。
//!
//! - [`Platform`]：线程与时钟（r8platform 提供标准库实现）
//! - [`Isolate`]：一个独立的 JS 世界，持有 GC 堆与剖析器
//! - [`Scope`]：一次进入 isolate 的作用域；其中的 [`Value`] 就是 V8 的 Local，离开作用域即不可见
//! - [`Global`]：持久句柄，跨多次 `enter` 存活，drop 即放掉
//! - [`Context`] / [`Realm`]：执行环境，带全局对象与各内建原型（[`Intrinsics`] 由 r8builtins 实现）
//! - [`Agent`]：一次运行的上下文，JS 运算语义挂在它上面；要调回 JS 时经 [`Invoke`]（执行引擎实现）
//! - [`Profiler`]：调用 / 回边计数
//! - [`Engine`]：执行引擎（r8executor）实现的统一入口
#![no_std]
extern crate alloc;

mod agent;
mod bigint;
mod engine;
mod error;
mod handle;
mod isolate;
mod object;
mod ops;
mod platform;
mod private;
mod proxy;
mod typed;
mod profiler;
mod property;
mod promise;
mod realm;
mod value;

pub use agent::{Agent, CodeKind, Invoke, Resume, Step};
pub use engine::{Engine, Source};
pub use error::Error;
pub use handle::{ContextRoot, Global};
pub use isolate::{CreateParams, Isolate, Scope};
pub use object::{ArgumentsMap, BigIntData, Bound, BufferData, Capability, Closure, ClosureFn, ElementKind, ViewData, HelperKind, HelperState, IteratorHelper, NativeClosure, ProxyData, Collection, MapKey, Request, JsBigInt, RegExpData, Coroutine, CoroutineState, Env, IterKind, IteratorState, Job, JsEnv, JsObject, JsString, JsSymbol, Native, NativeFn, ObjectData, ObjectKind, Property, Reaction, Settled, Slot, Suspended, SymbolData};
pub use bigint::Numeric;
pub use ops::{BinaryOp, CompareOp, ErrorKind, Hint, IterRecord, UnaryOp};
pub use platform::{Platform, Task};
pub use profiler::{Counter, FuncId, Profiler};
pub use typed::{Element, canonical_numeric, f16_to_f64, f64_to_f16};
pub use property::{Descriptor, Integrity, Key, PropertyKey};
pub use realm::{Context, Intrinsics, Realm, WellKnown};
pub use value::Value;
