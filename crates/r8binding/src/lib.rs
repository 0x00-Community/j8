//! r8binding：Rust ⇄ JS 绑定（对标 V8 的 FunctionTemplate / ObjectTemplate）。
//!
//! - [`FromJs`] / [`IntoJs`]：实参与返回值的转换（数、布尔、字符串、`Option`、`Vec`、对象、`Result`……）
//! - [`Class`]：把一个 Rust 类型导出成 JS 类；一般不手写，用 [`class`] 属性宏写在 `impl` 块上生成
//! - 宿主对象：JS 对象里放一份 Rust 值（[`ObjectKind::Host`]），方法里按 `&self` / `&mut self` 取出
//!
//! ```ignore
//! struct Counter { n: f64 }
//!
//! #[r8binding::class]
//! impl Counter {
//!     #[js(constructor)]
//!     fn new(start: Option<f64>) -> Self { Self { n: start.unwrap_or(0.0) } }
//!     fn inc(&mut self, by: f64) -> f64 { self.n += by; self.n }
//!     #[js(getter)]
//!     fn value(&self) -> f64 { self.n }
//!     fn zero() -> f64 { 0.0 }            // 没有 self：静态方法
//! }
//!
//! let handle = Counter::install(&mut agent, global)?;   // 全局 `Counter`
//! ```
#![no_std]
extern crate alloc;
extern crate self as r8binding;

mod class;
mod convert;

pub use class::{Class, ClassHandle, Member};
pub use convert::{FromJs, IntoJs, Rest, This};
pub use r8macros::class;

#[doc(hidden)]
pub use r8frame as frame;
#[doc(hidden)]
pub use class::Glue;
