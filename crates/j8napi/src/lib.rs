//! j8napi: Node-API（napi）在 j8 上的实现——原生插件（包括 napi-rs 写的模块）链接的那套 C 接口。
//!
//! ```text
//! 插件 ── napi_* (C ABI，本 crate 导出) ──▶ Env（句柄栈、作用域、挂起的异常、终结队列）──▶ j8framework::Agent
//! ```
//!
//! 嵌入方在某次进入里用 [`Env::open`] 打开一个插件（给它注册函数），拿回 exports。插件的 JS 函数是能 `new` 的
//! [`j8framework::NativeClosure`]；`napi_wrap` / 类型标签放在对象的私有成员里；被回收的宿主对象把终结回调排进队列，下次进
//! 回调时在 JS 线程上跑。
//!
//! 已知差异：引用计数降到 0 的 `napi_ref` 仍是强引用（堆还没有弱引用）；字符串按 UTF-8 存，UTF-16 里落单的代理项会丢；
//! Buffer 就是 Uint8Array；异步工作与线程安全函数要事件循环，`unref` 不让循环提前结束，没有 libuv 循环。
#![allow(clippy::missing_safety_doc, reason = "Node-API 的入口：安全约定就是 Node-API 文档")]
extern crate alloc;

mod abi;
mod callback;
mod convert;
mod env;
mod types;

pub use callback::CallbackInfo;
pub use env::{Env, Finalizer, Pending};
pub use types::{Attributes, Callback, CleanupHook, ExtendedErrorInfo, NodeVersion, Finalize, Handle, KeyConversion, KeyFilter, KeyMode, PropertyDescriptor, RegisterFn, Status, TypeTag, TypedArrayType, ValueType};

/// `NAPI_AUTO_LENGTH`：字符串以 NUL 结尾，长度自己数
pub const AUTO_LENGTH: usize = usize::MAX;
/// 支持的 Node-API 版本（`napi_get_version`）
pub const NAPI_VERSION: u32 = 9;

/// 一组 Node-API 入口：`env` 为空返回 `InvalidArg`；函数体返回 `Result<(), Status>`，结尾记下错误信息。
macro_rules! napi {
    ($( $(#[$m:meta])* fn $name:ident($env:ident $(, $arg:ident: $ty:ty)* $(,)?) $body:block )*) => {$(
        $(#[$m])*
        #[unsafe(no_mangle)]
        pub unsafe extern "C" fn $name(env: *mut $crate::Env $(, $arg: $ty)*) -> $crate::Status {
            // SAFETY: `Env::open` 给出去的指针（空的拒掉）
            let Some($env) = (unsafe { env.as_ref() }) else { return $crate::Status::InvalidArg };
            #[allow(clippy::redundant_closure_call, unused_unsafe, reason = "函数体里用 `?`")]
            let r: Result<(), $crate::Status> = (|| $body)();
            $env.done(r)
        }
    )*};
}
use napi;
