use alloc::vec::Vec;
use core::ffi::c_void;
use core::ptr::NonNull;

use j8framework::{Scope, Error, JsObject, NativeClosure, ObjectData, ObjectKind, Value};

use crate::{Env, Handle};

#[derive(Debug)]
pub struct CallbackInfo {
    pub this: Handle,
    pub args: Vec<Handle>,
    pub new_target: Handle,
    pub data: *mut c_void,
}

#[derive(Clone, Copy, Debug)]
struct Hook<'e> {
    env: NonNull<Env<'e>>,
    cb: unsafe extern "C" fn(env: *mut Env<'_>, info: *mut CallbackInfo) -> Handle,
    data: *mut c_void,
}

unsafe impl Send for Hook<'_> {}

unsafe impl Sync for Hook<'_> {}

unsafe impl<'e> crate::Opaque<'e> for Hook<'e> {
    type Brand = HookBrand;
}

pub enum HookBrand {}

impl Env<'_> {
    pub fn function<'scope>(&self, a: &Scope<'scope, '_>, name: &str, cb: unsafe extern "C" fn(*mut Env<'_>, *mut CallbackInfo) -> Handle, data: *mut c_void, constructor: bool) -> JsObject<'scope> {
        let hook = Self::host(a, Hook { env: NonNull::from(self).cast::<Env<'scope>>(), cb, data });
        let f = ObjectData {
            kind: ObjectKind::NativeClosure(NativeClosure { f: Self::trampoline, slots: alloc::vec![hook.into()], constructor }),
            proto: Some(a.context.function_proto),
            ..ObjectData::default()
        }
        .alloc(a.mc);
        a.context.name_function(a.mc, f, name, Some(0));
        f
    }

    fn trampoline<'scope>(a: &mut Scope<'scope, '_>, me: JsObject<'scope>, this: Value<'scope>, args: &[Value<'scope>]) -> Result<Value<'scope>, Error> {
        let hook = match &me.borrow().kind {
            ObjectKind::NativeClosure(c) => c.slots.first().and_then(|v| v.as_object()).and_then(|h| Self::hosted::<Hook<'scope>, _>(a, h, |h| *h)),
            _ => None,
        };
        let Some(hook) = hook else { unreachable!("napi functions carry their hook") };
        let new_target = a.new_target;
        let env = unsafe { hook.env.as_ref() };
        env.enter(a, |env| {
            env.run_finalizers();
            let mut info = CallbackInfo {
                this: env.push(this),
                args: args.iter().map(|&v| env.push(v)).collect(),
                new_target: if new_target == Value::Undefined { Handle::default() } else { env.push(new_target) },
                data: hook.data,
            };
            let r = unsafe { (hook.cb)(env.as_ptr(), &raw mut info) };
            let v = env.value(r).unwrap_or(Value::Undefined);
            env.take_pending().map_or(Ok(v), Err)
        })
    }
}
