//! r8：嵌入方用的门面。绑定宏与它们生成的代码要用的东西都从这里取：
//!
//! - `#[r8::class]`：`impl` 块导出成 JS 类（构造器、原型方法、访问器、静态方法）
//! - `#[r8::object]`：只有关联函数的 `impl` 块导出成普通对象（`console` / `process` 这类）
//! - `#[r8::function]`：一个函数导出成 JS 函数
//!
//! 用法见 `crates/macros/USAGE.md`。
#![no_std]

pub use r8binding::{Class, ClassHandle, FromJs, IntoJs, Member, Object, Rest, This};
pub use r8macros::{class, function, object};

#[doc(hidden)]
pub use r8binding::Glue;
pub use r8frame as frame;
