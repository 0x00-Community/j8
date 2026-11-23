use core::ffi::{c_char, c_void};

use j8framework::{Error, ErrorKind, Value};

use crate::convert::Ffi;
use crate::{Env, ExtendedErrorInfo, Handle, Status, napi};

impl Env {
    fn make_error<'scope>(&self, a: &mut j8framework::Agent<'scope, '_>, kind: ErrorKind, code: Option<Value<'scope>>, msg: &str) -> Result<Value<'scope>, Status> {
        let e = a.error(kind, msg);
        if let Some(code) = code {
            self.js(a.set_named(e.into(), "code", code))?;
        }
        Ok(e.into())
    }

    fn create_error(&self, kind: ErrorKind, code: Handle, msg: Handle, result: *mut Handle) -> Result<(), Status> {
        let a = self.agent()?;
        let msg = self.string(msg)?;
        let code = if code.0.is_none() { None } else { Some(self.string(code)?.into()) };
        let e = self.make_error(a, kind, code, &msg)?;
        self.out(result, e)
    }

    fn throw_error(&self, kind: ErrorKind, code: *const c_char, msg: *const c_char) -> Result<(), Status> {
        self.preamble()?;
        let a = self.agent()?;
        let msg = Ffi::utf8(msg, crate::AUTO_LENGTH)?;
        let code = if code.is_null() { None } else { Some(a.string(&Ffi::utf8(code, crate::AUTO_LENGTH)?).into()) };
        let e = self.make_error(a, kind, code, &msg)?;
        self.throw(a.throw_value(e));
        Ok(())
    }
}

napi! {
    fn napi_create_error(env, code: Handle, msg: Handle, result: *mut Handle) {
        env.create_error(ErrorKind::Error, code, msg, result)
    }

    fn napi_create_type_error(env, code: Handle, msg: Handle, result: *mut Handle) {
        env.create_error(ErrorKind::Type, code, msg, result)
    }

    fn napi_create_range_error(env, code: Handle, msg: Handle, result: *mut Handle) {
        env.create_error(ErrorKind::Range, code, msg, result)
    }

    fn node_api_create_syntax_error(env, code: Handle, msg: Handle, result: *mut Handle) {
        env.create_error(ErrorKind::Syntax, code, msg, result)
    }

    fn napi_throw(env, error: Handle) {
        env.preamble()?;
        let a = env.agent()?;
        env.throw(a.throw_value(env.value(error)?));
        Ok(())
    }

    fn napi_throw_error(env, code: *const c_char, msg: *const c_char) {
        env.throw_error(ErrorKind::Error, code, msg)
    }

    fn napi_throw_type_error(env, code: *const c_char, msg: *const c_char) {
        env.throw_error(ErrorKind::Type, code, msg)
    }

    fn napi_throw_range_error(env, code: *const c_char, msg: *const c_char) {
        env.throw_error(ErrorKind::Range, code, msg)
    }

    fn node_api_throw_syntax_error(env, code: *const c_char, msg: *const c_char) {
        env.throw_error(ErrorKind::Syntax, code, msg)
    }

    fn napi_is_exception_pending(env, result: *mut bool) {
        Ffi::put(result, env.has_pending())
    }

    fn napi_get_and_clear_last_exception(env, result: *mut Handle) {
        let a = env.agent()?;
        let v = match env.take_pending() {
            Some(Error::Exception) => a.take_exception(),
            Some(e @ Error::Terminated) => {
                env.throw(e);
                Value::Undefined
            }
            Some(Error::Syntax(m) | Error::Unsupported(m)) => a.error(ErrorKind::Syntax, &m).into(),
            None => Value::Undefined,
        };
        env.out(result, v)
    }

    fn napi_open_handle_scope(env, result: *mut *mut c_void) {
        Ffi::put(result, env.open_scope(false))
    }

    fn napi_close_handle_scope(env, scope: *mut c_void) {
        env.close_scope(scope)
    }

    fn napi_open_escapable_handle_scope(env, result: *mut *mut c_void) {
        Ffi::put(result, env.open_scope(true))
    }

    fn napi_close_escapable_handle_scope(env, scope: *mut c_void) {
        env.close_scope(scope)
    }

    fn napi_escape_handle(env, scope: *mut c_void, escapee: Handle, result: *mut Handle) {
        let h = env.escape(scope, env.value(escapee)?)?;
        Ffi::put(result, h)
    }
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn napi_get_last_error_info(env: *mut Env, result: *mut *const ExtendedErrorInfo) -> Status {
    let Some(env) = (unsafe { env.as_ref() }) else { return Status::InvalidArg };
    match Ffi::put(result, env.last_error()) {
        Ok(()) => Status::Ok,
        Err(s) => s,
    }
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn napi_fatal_error(location: *const c_char, location_len: usize, message: *const c_char, message_len: usize) -> ! {
    let location = Ffi::utf8(location, location_len).unwrap_or_default();
    let message = Ffi::utf8(message, message_len).unwrap_or_default();
    std::eprintln!("FATAL ERROR: {location} {message}");
    std::process::abort()
}
