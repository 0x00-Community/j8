#![allow(clippy::missing_safety_doc, reason = "Node-API entry points: the safety contract is the Node-API documentation")]
extern crate alloc;

mod abi;
mod callback;
mod convert;
mod env;
mod types;

pub use callback::CallbackInfo;
pub use env::{Env, Finalizer, Pending};
pub use types::{Attributes, Callback, CleanupHook, ExtendedErrorInfo, NodeVersion, Finalize, Handle, KeyConversion, KeyFilter, KeyMode, PropertyDescriptor, RegisterFn, Status, TypeTag, TypedArrayType, ValueType};

pub const AUTO_LENGTH: usize = usize::MAX;

pub const NAPI_VERSION: u32 = 9;

macro_rules! napi {
    ($( $(#[$m:meta])* fn $name:ident($env:ident $(, $arg:ident: $ty:ty)* $(,)?) $body:block )*) => {$(
        $(#[$m])*
        #[unsafe(no_mangle)]
        pub unsafe extern "C" fn $name(env: *mut $crate::Env $(, $arg: $ty)*) -> $crate::Status {
            let Some($env) = (unsafe { env.as_ref() }) else { return $crate::Status::InvalidArg };
            #[allow(clippy::redundant_closure_call, unused_unsafe, reason = "`?` inside the function body")]
            let r: Result<(), $crate::Status> = (|| $body)();
            $env.done(r)
        }
    )*};
}
use napi;
