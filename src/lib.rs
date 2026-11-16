//! j8: the facade for embedders. The binding macros and everything their generated code needs come from here:
//!
//! - `#[j8::class]`: exports an `impl` block as a JS class (constructor, prototype methods, accessors, static methods)
//! - `#[j8::object]`: exports an `impl` block of associated functions only as a plain object
//!   (like `console` / `process`)
//! - `#[j8::function]`: exports a function as a JS function
//!
//! See `crates/macros/USAGE.md` for usage.
#![no_std]

pub use j8binding::{Class, ClassHandle, FromJs, IntoJs, Member, Object, Rest, This};
pub use j8macros::{class, function, object};

#[doc(hidden)]
pub use j8binding::Glue;
pub use j8framework as frame;
