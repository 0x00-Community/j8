extern crate alloc;

mod buffer;
mod class;
mod convert;
mod handles;
mod module;

pub use buffer::{ArrayBuffer, BufferRef, Shared, Transfer};
pub use class::{Class, ClassHandle, Member, Object};
pub use convert::{FromJs, IntoJs, Rest, This};
pub use handles::{Bytes, Function, Persistent, Promise};
#[cfg(feature = "json")]
pub use handles::Json;
pub use module::{Install, Module, NATIVE};

pub use j8framework::{Backing, External, SharedMemory};
#[doc(hidden)]
pub use j8framework as frame;
#[doc(hidden)]
pub use class::Glue;
