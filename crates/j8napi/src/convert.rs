use alloc::borrow::Cow;
use alloc::string::String;
use core::ffi::c_char;

use j8framework::{Scope, JsObject, JsString, Key, PropertyKey, Value, ValueKind};

use crate::{AUTO_LENGTH, Env, Handle, Status};

pub enum Name<'scope> {
    Str(JsString<'scope>),
    Sym(j8framework::JsSymbol<'scope>),
}

impl<'scope> Name<'scope> {
    pub fn key(&self) -> Key<'_, 'scope> {
        match self {
            Self::Str(s) => Key::Str(s),
            Self::Sym(s) => Key::Sym(*s),
        }
    }
}

impl<'scope> From<PropertyKey<'scope>> for Name<'scope> {
    fn from(k: PropertyKey<'scope>) -> Self {
        match k {
            PropertyKey::String(s) => Self::Str(s),
            PropertyKey::Symbol(s) => Self::Sym(s),
        }
    }
}

pub struct Ffi;

impl Ffi {
    pub fn put<T>(out: *mut T, v: T) -> Result<(), Status> {
        if out.is_null() {
            return Err(Status::InvalidArg);
        }
        unsafe { out.write(v) };
        Ok(())
    }

    pub fn put_opt<T>(out: *mut T, v: T) {
        if !out.is_null() {
            unsafe { out.write(v) };
        }
    }

    pub fn utf8<'s>(ptr: *const c_char, len: usize) -> Result<Cow<'s, str>, Status> {
        Ok(String::from_utf8_lossy(Self::bytes(ptr, len)?))
    }

    pub fn bytes<'s>(ptr: *const c_char, len: usize) -> Result<&'s [u8], Status> {
        if ptr.is_null() {
            return if len == 0 || len == AUTO_LENGTH { Ok(&[]) } else { Err(Status::InvalidArg) };
        }
        if len == AUTO_LENGTH {
            return Ok(unsafe { core::ffi::CStr::from_ptr(ptr) }.to_bytes());
        }
        Ok(unsafe { core::slice::from_raw_parts(ptr.cast::<u8>(), len) })
    }

    pub fn slice<'s, T>(ptr: *const T, n: usize) -> Result<&'s [T], Status> {
        if n == 0 {
            return Ok(&[]);
        }
        if ptr.is_null() {
            return Err(Status::InvalidArg);
        }
        Ok(unsafe { core::slice::from_raw_parts(ptr, n) })
    }
}

impl Env<'_> {
    pub fn values<'scope>(&self, ptr: *const Handle, n: usize) -> Result<alloc::vec::Vec<Value<'scope>>, Status> {
        Ffi::slice(ptr, n)?.iter().map(|&h| self.value(h)).collect()
    }

    pub fn object<'scope>(&self, h: Handle) -> Result<JsObject<'scope>, Status> {
        self.value(h)?.as_object().ok_or(Status::ObjectExpected)
    }

    pub fn to_object<'scope>(&self, a: &Scope<'scope, '_>, h: Handle) -> Result<JsObject<'scope>, Status> {
        a.to_object(self.value(h)?).map_err(|_| Status::ObjectExpected)
    }

    pub fn string<'scope>(&self, h: Handle) -> Result<JsString<'scope>, Status> {
        match self.value(h)?.kind() {
            ValueKind::String(s) => Ok(s),
            _ => Err(Status::StringExpected),
        }
    }

    pub fn number(&self, h: Handle) -> Result<f64, Status> {
        self.value(h)?.as_number().ok_or(Status::NumberExpected)
    }

    pub fn callable<'scope>(&self, a: &Scope<'scope, '_>, h: Handle) -> Result<Value<'scope>, Status> {
        let v = self.value(h)?;
        if a.is_callable(v) { Ok(v) } else { Err(Status::FunctionExpected) }
    }

    pub fn name<'scope>(&self, h: Handle) -> Result<Name<'scope>, Status> {
        match self.value(h)?.kind() {
            ValueKind::String(s) => Ok(Name::Str(s)),
            ValueKind::Symbol(s) => Ok(Name::Sym(s)),
            _ => Err(Status::NameExpected),
        }
    }
}
