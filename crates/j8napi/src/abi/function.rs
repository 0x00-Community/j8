//! 函数与类：创建、调用、`new`、回调信息、`napi_define_class`。
use core::ffi::{c_char, c_void};

use j8framework::{Descriptor, Key, Property};

use crate::convert::Ffi;
use crate::{Attributes, Callback, CallbackInfo, Env, Handle, PropertyDescriptor, Status, napi};

impl Env {
    /// C 字符串名字（空指针是空名）
    fn label<'s>(name: *const c_char, length: usize) -> Result<alloc::borrow::Cow<'s, str>, Status> {
        if name.is_null() { Ok(alloc::borrow::Cow::Borrowed("")) } else { Ffi::utf8(name, length) }
    }
}

napi! {
    fn napi_create_function(env, utf8name: *const c_char, length: usize, cb: Callback, data: *mut c_void, result: *mut Handle) {
        let a = env.agent()?;
        let cb = cb.ok_or(Status::InvalidArg)?;
        let name = Env::label(utf8name, length)?;
        env.out(result, env.function(a, &name, cb, data, false).into())
    }

    fn napi_define_class(
        env,
        utf8name: *const c_char,
        length: usize,
        constructor: Callback,
        data: *mut c_void,
        property_count: usize,
        properties: *const PropertyDescriptor,
        result: *mut Handle,
    ) {
        env.preamble()?;
        let a = env.agent()?;
        let cb = constructor.ok_or(Status::InvalidArg)?;
        let name = Env::label(utf8name, length)?;
        let ctor = env.function(a, &name, cb, data, true);
        let proto = a.object();
        proto.borrow_mut(a.mc).define("constructor", Property::hidden(ctor.into()));
        let fixed = Descriptor { value: Some(proto.into()), writable: Some(false), enumerable: Some(false), configurable: Some(false), ..Descriptor::default() };
        env.js(a.define_property_or_throw(ctor, Key::Str("prototype"), fixed))?;
        for p in Ffi::slice(properties, property_count)? {
            let target = if p.attributes.has(Attributes::STATIC) { ctor } else { proto };
            env.define(a, target, p)?;
        }
        env.out(result, ctor.into())
    }

    fn napi_get_cb_info(env, cbinfo: *mut CallbackInfo, argc: *mut usize, argv: *mut Handle, this_arg: *mut Handle, data: *mut *mut c_void) {
        // SAFETY: 回调拿到的 `info`，回调返回前有效
        let info = unsafe { cbinfo.as_ref() }.ok_or(Status::InvalidArg)?;
        if !argv.is_null() {
            if argc.is_null() {
                return Err(Status::InvalidArg);
            }
            // SAFETY: 输入时是 `argv` 的长度
            let cap = unsafe { argc.read() };
            let undefined = env.push(j8framework::Value::Undefined);
            for i in 0..cap {
                // SAFETY: 调用方给的 `cap` 个槽
                unsafe { argv.add(i).write(info.args.get(i).copied().unwrap_or(undefined)) };
            }
        }
        Ffi::put_opt(argc, info.args.len());
        Ffi::put_opt(this_arg, info.this);
        Ffi::put_opt(data, info.data);
        Ok(())
    }

    fn napi_get_new_target(env, cbinfo: *mut CallbackInfo, result: *mut Handle) {
        let _ = env;
        // SAFETY: 回调拿到的 `info`
        let info = unsafe { cbinfo.as_ref() }.ok_or(Status::InvalidArg)?;
        Ffi::put(result, info.new_target)
    }

    fn napi_call_function(env, recv: Handle, func: Handle, argc: usize, argv: *const Handle, result: *mut Handle) {
        env.preamble()?;
        let a = env.agent()?;
        let this = env.value(recv)?;
        let f = env.callable(a, func)?;
        let args = env.values(argv, argc)?;
        let v = env.js(a.call(f, this, &args))?;
        if !result.is_null() {
            env.out(result, v)?;
        }
        Ok(())
    }

    fn napi_new_instance(env, constructor: Handle, argc: usize, argv: *const Handle, result: *mut Handle) {
        env.preamble()?;
        let a = env.agent()?;
        let c = env.value(constructor)?;
        if !a.is_constructor(c) {
            return Err(Status::FunctionExpected);
        }
        let args = env.values(argv, argc)?;
        let v = env.js(a.construct(c, &args, c))?;
        env.out(result, v)
    }
}
