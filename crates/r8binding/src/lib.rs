//! r8binding：Rust ⇄ JS 绑定（对标 V8 的 FunctionTemplate / ObjectTemplate）。
//!
//! - [`FromJs`] / [`IntoJs`]：实参与返回值的转换（数、布尔、字符串、`Option`、`Vec`、对象、`Result`……）
//! - [`Class`] / [`Object`]：把 Rust 类型导出成 JS 类 / 普通对象；一般不手写，经门面 crate `r8` 的
//!   `#[r8::class]` / `#[r8::object]` / `#[r8::function]` 生成
//! - 宿主对象：JS 对象里放一份 Rust 值（[`ObjectKind::Host`]），方法里按 `&self` / `&mut self` 取出
//!
//! ```ignore
//! struct Counter { n: f64 }
//!
//! #[r8::class]
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

mod class;
mod convert;

pub use class::{Class, ClassHandle, Member, Object};
pub use convert::{FromJs, IntoJs, Rest, This};

#[doc(hidden)]
pub use r8framework as frame;
#[doc(hidden)]
pub use class::Glue;
