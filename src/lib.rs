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
