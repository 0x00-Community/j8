//! r8: the facade for embedders. The binding macros and everything their generated code needs come from here:
//!
//! - `#[r8::class]`: exports an `impl` block as a JS class (constructor, prototype methods, accessors, static methods)
//! - `#[r8::object]`: exports an `impl` block of associated functions only as a plain object
//!   (like `console` / `process`)
//! - `#[r8::function]`: exports a function as a JS function
//!
//! See `crates/macros/USAGE.md` for usage.
#![no_std]

pub use r8binding::{Class, ClassHandle, FromJs, IntoJs, Member, Object, Rest, This};
pub use r8macros::{class, function, object};

#[doc(hidden)]
pub use r8binding::Glue;
pub use r8framework as frame;
