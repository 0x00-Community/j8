use alloc::boxed::Box;
use core::ffi::c_void;

use j8framework::{Backing, BufferData, CodeKind, ElementKind, External, JsObject, ObjectData, ObjectKind, ScopeGlobal, Value, ValueBrand, ViewData};

use crate::convert::Ffi;
use crate::{Env, Finalize, Handle, Status, TypedArrayType, napi};

pub struct Deferred<'e> {
    resolve: ScopeGlobal<'e, ValueBrand>,
    reject: ScopeGlobal<'e, ValueBrand>,
}

impl Env<'_> {
    fn array_buffer<'scope>(a: &j8framework::Agent<'scope, '_>, bytes: Backing) -> JsObject<'scope> {
        let data = BufferData { bytes: Some(bytes), max: None, shared: false };
        ObjectData { kind: ObjectKind::ArrayBuffer(Box::new(data)), proto: Some(a.context.array_buffer_proto), ..ObjectData::default() }.alloc(a.mc)
    }

    fn buffer_bytes<'scope>(a: &j8framework::Agent<'scope, '_>, buffer: JsObject<'scope>) -> (*mut c_void, usize) {
        match &mut buffer.borrow_mut(a.mc).kind {
            ObjectKind::ArrayBuffer(b) => b.bytes.as_deref_mut().map_or((core::ptr::null_mut(), 0), |s| (s.as_mut_ptr().cast(), s.len())),
            _ => (core::ptr::null_mut(), 0),
        }
    }

    fn array_buffer_of<'scope>(&self, h: Handle) -> Result<JsObject<'scope>, Status> {
        let o = self.value(h)?.as_object().ok_or(Status::ArraybufferExpected)?;
        let ok = matches!(&o.borrow().kind, ObjectKind::ArrayBuffer(b) if !b.shared);
        if ok { Ok(o) } else { Err(Status::ArraybufferExpected) }
    }

    fn check_view<'scope>(&self, a: &j8framework::Agent<'scope, '_>, buffer: JsObject<'scope>, offset: usize, bytes: usize, size: usize, what: &str) -> Result<(), Status> {
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

    fn view_of<'scope>(&self, h: Handle) -> Result<(Option<ElementKind>, ViewData<'scope>), Status> {
        let o = self.value(h)?.as_object().ok_or(Status::InvalidArg)?;
        match &o.borrow().kind {
            ObjectKind::TypedArray(k, v) => Ok((Some(*k), *v)),
            ObjectKind::DataView(v) => Ok((None, *v)),
            _ => Err(Status::InvalidArg),
        }
    }
}

napi! {
    fn napi_create_arraybuffer(env, byte_length: usize, data: *mut *mut c_void, result: *mut Handle) {
        let a = env.agent()?;
        let mut bytes = alloc::vec![0u8; byte_length];
        Ffi::put_opt(data, bytes.as_mut_ptr().cast());
        env.out(result, Env::array_buffer(a, bytes.into()).into())
    }

    fn napi_create_external_arraybuffer(env, external_data: *mut c_void, byte_length: usize, finalize_cb: Finalize, finalize_hint: *mut c_void, result: *mut Handle) {
        let a = env.agent()?;
        let owner = Box::new(env.finalizer(finalize_cb, external_data, finalize_hint));
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

    fn napi_create_typedarray(env, r#type: i32, length: usize, arraybuffer: Handle, byte_offset: usize, result: *mut Handle) {
        env.preamble()?;
        let a = env.agent()?;
        let kind: ElementKind = TypedArrayType::try_from(r#type)?.into();
        let buffer = env.array_buffer_of(arraybuffer).map_err(|_| Status::InvalidArg)?;
        let bytes = length.checked_mul(kind.size()).ok_or(Status::InvalidArg)?;
        env.check_view(a, buffer, byte_offset, bytes, kind.size(), kind.view_name())?;
        let at = ElementKind::ALL.iter().position(|&k| k == kind).ok_or(Status::InvalidArg)?;
        let view = ViewData { buffer, offset: byte_offset, length: Some(length) };
        let o = ObjectData { kind: ObjectKind::TypedArray(kind, view), proto: Some(a.context.typed_protos[at]), ..ObjectData::default() }.alloc(a.mc);
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
        let o = ObjectData { kind: ObjectKind::DataView(view), proto: Some(a.context.data_view_proto), ..ObjectData::default() }.alloc(a.mc);
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

    fn napi_create_date(env, time: f64, result: *mut Handle) {
        let a = env.agent()?;
        let o = ObjectData { kind: ObjectKind::Date(time), proto: Some(a.context.date_proto), ..ObjectData::default() }.alloc(a.mc);
        env.out(result, o.into())
    }

    fn napi_get_date_value(env, value: Handle, result: *mut f64) {
        let o = env.value(value)?.as_object().ok_or(Status::DateExpected)?;
        let ObjectKind::Date(t) = o.borrow().kind else { return Err(Status::DateExpected) };
        Ffi::put(result, t)
    }

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

    fn napi_run_script(env, script: Handle, result: *mut Handle) {
        env.preamble()?;
        let a = env.agent()?;
        let src = env.string(script)?;
        let v = env.js(a.eval_code(&src, CodeKind::Script))?;
        env.out(result, v)
    }
}

impl Env<'_> {
    fn settle(&self, deferred: *mut Deferred, v: Handle, reject: bool) -> Result<(), Status> {
        let a = self.agent()?;
        if deferred.is_null() {
            return Err(Status::InvalidArg);
        }
        let d = unsafe { Box::from_raw(deferred) };
        let f = a.local(if reject { &d.reject } else { &d.resolve });
        let v: Value<'_> = self.value(v)?;
        self.js(a.call(f, Value::Undefined, &[v])).map(drop)
    }
}
