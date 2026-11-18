//! 原生回调变成 JS 函数：函数对象是一个 [`NativeClosure`]，槽 0 是装着 [`Hook`] 的宿主对象；JS 调它时进 [`Env::enter`]，
//! 压好 `this`、实参、`new.target`，调 C 回调，取回结果和挂起的异常。普通函数、getter / setter、构造函数（`this` 由执行层
//! 按 `new.target` 先造好）都走这一个入口。
use alloc::vec::Vec;
use core::ffi::c_void;
use core::ptr::NonNull;

use j8framework::{Agent, Error, JsObject, NativeClosure, ObjectData, ObjectKind, Value};

use crate::{Env, Handle};

/// 一次回调的信息（`napi_callback_info`）
#[derive(Debug)]
pub struct CallbackInfo {
    pub this: Handle,
    pub args: Vec<Handle>,
    /// 构造调用时是 `new.target`，否则为空
    pub new_target: Handle,
    pub data: *mut c_void,
}

/// 一个 JS 函数背后的 C 回调
#[derive(Clone, Copy, Debug)]
struct Hook {
    env: NonNull<Env>,
    cb: unsafe extern "C" fn(env: *mut Env, info: *mut CallbackInfo) -> Handle,
    data: *mut c_void,
}

// SAFETY: 只在所属 isolate 的 JS 线程上用
unsafe impl Send for Hook {}
// SAFETY: 同上
unsafe impl Sync for Hook {}

impl Env {
    /// 一个调 `cb` 的 JS 函数（`constructor`：能被 `new`）
    pub fn function<'gc>(&self, a: &Agent<'gc, '_>, name: &str, cb: unsafe extern "C" fn(*mut Env, *mut CallbackInfo) -> Handle, data: *mut c_void, constructor: bool) -> JsObject<'gc> {
        let hook = Self::host(a, Hook { env: NonNull::from(self), cb, data });
        let f = ObjectData {
            kind: ObjectKind::NativeClosure(NativeClosure { f: Self::trampoline, slots: alloc::vec![hook.into()], constructor }),
            proto: Some(a.realm.function_proto),
            ..ObjectData::default()
        }
        .alloc(a.mc);
        a.realm.name_function(a.mc, f, name, Some(0));
        f
    }

    /// JS 调进来：转给 C 回调
    fn trampoline<'gc>(a: &mut Agent<'gc, '_>, me: JsObject<'gc>, this: Value<'gc>, args: &[Value<'gc>]) -> Result<Value<'gc>, Error> {
        let hook = match &me.borrow().kind {
            ObjectKind::NativeClosure(c) => c.slots.first().and_then(|v| v.as_object()).and_then(|h| Self::hosted::<Hook, _>(h, |h| *h)),
            _ => None,
        };
        let Some(hook) = hook else { unreachable!("napi functions carry their hook") };
        let new_target = a.new_target;
        // SAFETY: Env 随 global 对象活，函数对象能被调说明 context 还在
        let env = unsafe { hook.env.as_ref() };
        env.enter(a, |env| {
            env.run_finalizers();
            let mut info = CallbackInfo {
                this: env.push(this),
                args: args.iter().map(|&v| env.push(v)).collect(),
                new_target: if new_target == Value::Undefined { Handle::default() } else { env.push(new_target) },
                data: hook.data,
            };
            // SAFETY: 插件的回调，按 Node-API 的约定调
            let r = unsafe { (hook.cb)(env.as_ptr(), &raw mut info) };
            let v = env.value(r).unwrap_or(Value::Undefined);
            env.take_pending().map_or(Ok(v), Err)
        })
    }
}
