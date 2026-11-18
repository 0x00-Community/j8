//! 二进制数据（ArrayBuffer、typed array、DataView）、Date、Promise、跑脚本。
use alloc::boxed::Box;
use core::ffi::c_void;

use j8framework::{Backing, BufferData, CodeKind, ElementKind, External, Global, JsObject, ObjectData, ObjectKind, Value, ViewData};

use crate::convert::Ffi;
use crate::{Env, Finalize, Handle, Status, TypedArrayType, napi};

/// `napi_create_promise` 给出的 deferred：promise 的 resolve / reject
#[derive(Debug)]
pub struct Deferred {
    resolve: Global,
    reject: Global,
}

impl Env {
    /// 一个 ArrayBuffer 对象
    fn array_buffer<'gc>(a: &j8framework::Agent<'gc, '_>, bytes: Backing) -> JsObject<'gc> {
        let data = BufferData { bytes: Some(bytes), max: None, shared: false };
        ObjectData { kind: ObjectKind::ArrayBuffer(Box::new(data)), proto: Some(a.realm.array_buffer_proto), ..ObjectData::default() }.alloc(a.mc)
    }

    /// buffer 字节的起点与长度（detach 后为空、0）
    fn buffer_bytes<'gc>(a: &j8framework::Agent<'gc, '_>, buffer: JsObject<'gc>) -> (*mut c_void, usize) {
        match &mut buffer.borrow_mut(a.mc).kind {
            ObjectKind::ArrayBuffer(b) => b.bytes.as_deref_mut().map_or((core::ptr::null_mut(), 0), |s| (s.as_mut_ptr().cast(), s.len())),
            _ => (core::ptr::null_mut(), 0),
        }
    }

    /// 句柄里的（非共享）ArrayBuffer
    fn array_buffer_of<'gc>(&self, h: Handle) -> Result<JsObject<'gc>, Status> {
        let o = self.value(h)?.as_object().ok_or(Status::ArraybufferExpected)?;
        let ok = matches!(&o.borrow().kind, ObjectKind::ArrayBuffer(b) if !b.shared);
        if ok { Ok(o) } else { Err(Status::ArraybufferExpected) }
    }

    /// 在 `buffer` 上开视图的检查：越界或没对齐抛 RangeError（挂起）
    fn check_view<'gc>(&self, a: &j8framework::Agent<'gc, '_>, buffer: JsObject<'gc>, offset: usize, bytes: usize, size: usize, what: &str) -> Result<(), Status> {
        let len = Self::buffer_bytes(a, buffer).1;
        let msg = if !offset.is_multiple_of(size) {
            alloc::format!("start offset of {what} should be a multiple of {size}")
        } else if offset.checked_add(bytes).is_none_or(|end| end > len) {
            alloc::format!("Invalid {what} length")
        } else {
            return Ok(());
        };
        self.throw(a.throw(j8framework::ErrorKind::Range, &msg));
        Err(Status::PendingException)
    }

    /// 句柄里的视图（typed array 或 DataView）：种类与范围
    fn view_of<'gc>(&self, h: Handle) -> Result<(Option<ElementKind>, ViewData<'gc>), Status> {
        let o = self.value(h)?.as_object().ok_or(Status::InvalidArg)?;
        match &o.borrow().kind {
            ObjectKind::TypedArray(k, v) => Ok((Some(*k), *v)),
            ObjectKind::DataView(v) => Ok((None, *v)),
            _ => Err(Status::InvalidArg),
        }
    }
}

napi! {
    // ---- ArrayBuffer ----

    fn napi_create_arraybuffer(env, byte_length: usize, data: *mut *mut c_void, result: *mut Handle) {
        let a = env.agent()?;
        let mut bytes = alloc::vec![0u8; byte_length];
        Ffi::put_opt(data, bytes.as_mut_ptr().cast());
        env.out(result, Env::array_buffer(a, bytes.into()).into())
    }

    fn napi_create_external_arraybuffer(env, external_data: *mut c_void, byte_length: usize, finalize_cb: Finalize, finalize_hint: *mut c_void, result: *mut Handle) {
        let a = env.agent()?;
        let owner = Box::new(env.finalizer(finalize_cb, external_data, finalize_hint));
        // SAFETY: Node-API 约定：插件保证 `external_data` 在终结回调之前一直有效
        let e = unsafe { External::from_raw(external_data.cast(), byte_length, owner) };
        env.out(result, Env::array_buffer(a, e.into()).into())
    }

    fn napi_get_arraybuffer_info(env, arraybuffer: Handle, data: *mut *mut c_void, byte_length: *mut usize) {
        let a = env.agent()?;
        let (p, n) = Env::buffer_bytes(a, env.array_buffer_of(arraybuffer)?);
        Ffi::put_opt(data, p);
        Ffi::put_opt(byte_length, n);
        Ok(())
    }

    fn napi_detach_arraybuffer(env, arraybuffer: Handle) {
        let a = env.agent()?;
        let o = env.array_buffer_of(arraybuffer).map_err(|_| Status::DetachableArraybufferExpected)?;
        if let ObjectKind::ArrayBuffer(b) = &mut o.borrow_mut(a.mc).kind {
            b.bytes = None;
        }
        Ok(())
    }

    fn napi_is_detached_arraybuffer(env, value: Handle, result: *mut bool) {
        let detached = env.value(value)?.as_object().is_some_and(|o| matches!(&o.borrow().kind, ObjectKind::ArrayBuffer(b) if b.bytes.is_none()));
        Ffi::put(result, detached)
    }

    // ---- typed array / DataView ----

    fn napi_create_typedarray(env, r#type: i32, length: usize, arraybuffer: Handle, byte_offset: usize, result: *mut Handle) {
        env.preamble()?;
        let a = env.agent()?;
        let kind: ElementKind = TypedArrayType::try_from(r#type)?.into();
        let buffer = env.array_buffer_of(arraybuffer).map_err(|_| Status::InvalidArg)?;
        let bytes = length.checked_mul(kind.size()).ok_or(Status::InvalidArg)?;
        env.check_view(a, buffer, byte_offset, bytes, kind.size(), kind.view_name())?;
        let at = ElementKind::ALL.iter().position(|&k| k == kind).ok_or(Status::InvalidArg)?;
        let view = ViewData { buffer, offset: byte_offset, length: Some(length) };
        let o = ObjectData { kind: ObjectKind::TypedArray(kind, view), proto: Some(a.realm.typed_protos[at]), ..ObjectData::default() }.alloc(a.mc);
        env.out(result, o.into())
    }

    fn napi_get_typedarray_info(env, typedarray: Handle, r#type: *mut TypedArrayType, length: *mut usize, data: *mut *mut c_void, arraybuffer: *mut Handle, byte_offset: *mut usize) {
        let a = env.agent()?;
        let (Some(kind), view) = env.view_of(typedarray)? else { return Err(Status::InvalidArg) };
        let (p, _) = Env::buffer_bytes(a, view.buffer);
        Ffi::put_opt(r#type, kind.into());
        Ffi::put_opt(length, j8framework::Agent::view_length(&view, kind.size()).unwrap_or(0));
        Ffi::put_opt(data, if p.is_null() { p } else { p.wrapping_byte_add(view.offset) });
        Ffi::put_opt(byte_offset, view.offset);
        if !arraybuffer.is_null() {
            env.out(arraybuffer, view.buffer.into())?;
        }
        Ok(())
    }

    fn napi_create_dataview(env, byte_length: usize, arraybuffer: Handle, byte_offset: usize, result: *mut Handle) {
        env.preamble()?;
        let a = env.agent()?;
        let buffer = env.array_buffer_of(arraybuffer).map_err(|_| Status::InvalidArg)?;
        env.check_view(a, buffer, byte_offset, byte_length, 1, "DataView")?;
        let view = ViewData { buffer, offset: byte_offset, length: Some(byte_length) };
        let o = ObjectData { kind: ObjectKind::DataView(view), proto: Some(a.realm.data_view_proto), ..ObjectData::default() }.alloc(a.mc);
        env.out(result, o.into())
    }

    fn napi_get_dataview_info(env, dataview: Handle, bytelength: *mut usize, data: *mut *mut c_void, arraybuffer: *mut Handle, byte_offset: *mut usize) {
        let a = env.agent()?;
        let (None, view) = env.view_of(dataview)? else { return Err(Status::InvalidArg) };
        let (p, _) = Env::buffer_bytes(a, view.buffer);
        Ffi::put_opt(bytelength, j8framework::Agent::view_length(&view, 1).unwrap_or(0));
        Ffi::put_opt(data, if p.is_null() { p } else { p.wrapping_byte_add(view.offset) });
        Ffi::put_opt(byte_offset, view.offset);
        if !arraybuffer.is_null() {
            env.out(arraybuffer, view.buffer.into())?;
        }
        Ok(())
    }

    // ---- Date ----

    fn napi_create_date(env, time: f64, result: *mut Handle) {
        let a = env.agent()?;
        let o = ObjectData { kind: ObjectKind::Date(time), proto: Some(a.realm.date_proto), ..ObjectData::default() }.alloc(a.mc);
        env.out(result, o.into())
    }

    fn napi_get_date_value(env, value: Handle, result: *mut f64) {
        let o = env.value(value)?.as_object().ok_or(Status::DateExpected)?;
        let ObjectKind::Date(t) = o.borrow().kind else { return Err(Status::DateExpected) };
        Ffi::put(result, t)
    }

    // ---- Promise ----

    fn napi_create_promise(env, deferred: *mut *mut Deferred, promise: *mut Handle) {
        let a = env.agent()?;
        if deferred.is_null() {
            return Err(Status::InvalidArg);
        }
        let p = a.new_promise();
        let (resolve, reject) = a.promise_resolvers(p);
        env.out(promise, p.into())?;
        Ffi::put(deferred, Box::into_raw(Box::new(Deferred { resolve: a.global(resolve.into()), reject: a.global(reject.into()) })))
    }

    fn napi_resolve_deferred(env, deferred: *mut Deferred, resolution: Handle) {
        env.settle(deferred, resolution, false)
    }

    fn napi_reject_deferred(env, deferred: *mut Deferred, rejection: Handle) {
        env.settle(deferred, rejection, true)
    }

    // ---- 跑脚本 ----

    fn napi_run_script(env, script: Handle, result: *mut Handle) {
        env.preamble()?;
        let a = env.agent()?;
        let src = env.string(script)?;
        let v = env.js(a.eval_code(&src, CodeKind::Script))?;
        env.out(result, v)
    }
}

impl Env {
    /// 兑现 / 拒绝一个 deferred（用掉它）
    fn settle(&self, deferred: *mut Deferred, v: Handle, reject: bool) -> Result<(), Status> {
        let a = self.agent()?;
        if deferred.is_null() {
            return Err(Status::InvalidArg);
        }
        // SAFETY: `napi_create_promise` 给出的指针，只用一次
        let d = unsafe { Box::from_raw(deferred) };
        let f = a.local(if reject { &d.reject } else { &d.resolve });
        let v: Value<'_> = self.value(v)?;
        self.js(a.call(f, Value::Undefined, &[v])).map(drop)
    }
}
