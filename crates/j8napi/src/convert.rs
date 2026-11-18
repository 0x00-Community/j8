//! 入口里反复用到的转换：C 字符串 / 数组、句柄 ⇒ 某类 JS 值（类型不对给对应的返回码）。
use alloc::borrow::Cow;
use alloc::string::String;
use core::ffi::c_char;

use j8framework::{Agent, JsObject, JsString, Key, PropertyKey, Value, ValueKind};

use crate::{AUTO_LENGTH, Env, Handle, Status};

/// 属性键（字符串或 Symbol），借着字符串本身
pub enum Name<'gc> {
    Str(JsString<'gc>),
    Sym(j8framework::JsSymbol<'gc>),
}

impl<'gc> Name<'gc> {
    pub fn key(&self) -> Key<'_, 'gc> {
        match self {
            Self::Str(s) => Key::Str(s),
            Self::Sym(s) => Key::Sym(*s),
        }
    }
}

impl<'gc> From<PropertyKey<'gc>> for Name<'gc> {
    fn from(k: PropertyKey<'gc>) -> Self {
        match k {
            PropertyKey::String(s) => Self::Str(s),
            PropertyKey::Symbol(s) => Self::Sym(s),
        }
    }
}

/// 读写 C 指针的工具（不对外导出）
pub struct Ffi;

impl Ffi {
    /// 写 `v` 到 `out`（空指针拒掉）
    pub fn put<T>(out: *mut T, v: T) -> Result<(), Status> {
        if out.is_null() {
            return Err(Status::InvalidArg);
        }
        // SAFETY: 调用方给的输出指针，非空
        unsafe { out.write(v) };
        Ok(())
    }

    /// 写 `v` 到 `out`（空指针跳过）
    pub fn put_opt<T>(out: *mut T, v: T) {
        if !out.is_null() {
            // SAFETY: 调用方给的输出指针，非空
            unsafe { out.write(v) };
        }
    }

    /// C 的 UTF-8 字符串（`len` 为 [`AUTO_LENGTH`] 时以 NUL 结尾；坏字节换成 U+FFFD）
    pub fn utf8<'s>(ptr: *const c_char, len: usize) -> Result<Cow<'s, str>, Status> {
        Ok(String::from_utf8_lossy(Self::bytes(ptr, len)?))
    }

    /// C 字符串的字节
    pub fn bytes<'s>(ptr: *const c_char, len: usize) -> Result<&'s [u8], Status> {
        if ptr.is_null() {
            return if len == 0 || len == AUTO_LENGTH { Ok(&[]) } else { Err(Status::InvalidArg) };
        }
        if len == AUTO_LENGTH {
            // SAFETY: 调用方给的以 NUL 结尾的字符串
            return Ok(unsafe { core::ffi::CStr::from_ptr(ptr) }.to_bytes());
        }
        // SAFETY: 调用方给的 `len` 字节
        Ok(unsafe { core::slice::from_raw_parts(ptr.cast::<u8>(), len) })
    }

    /// C 数组（`n` 为 0 时可以是空指针）
    pub fn slice<'s, T>(ptr: *const T, n: usize) -> Result<&'s [T], Status> {
        if n == 0 {
            return Ok(&[]);
        }
        if ptr.is_null() {
            return Err(Status::InvalidArg);
        }
        // SAFETY: 调用方给的 `n` 个元素
        Ok(unsafe { core::slice::from_raw_parts(ptr, n) })
    }
}

impl Env {
    /// 一组句柄里的值
    pub fn values<'gc>(&self, ptr: *const Handle, n: usize) -> Result<alloc::vec::Vec<Value<'gc>>, Status> {
        Ffi::slice(ptr, n)?.iter().map(|&h| self.value(h)).collect()
    }

    /// 句柄里的对象（函数也是对象）
    pub fn object<'gc>(&self, h: Handle) -> Result<JsObject<'gc>, Status> {
        self.value(h)?.as_object().ok_or(Status::ObjectExpected)
    }

    /// 句柄里的值转成对象（ToObject；null / undefined 给 `ObjectExpected`）
    pub fn to_object<'gc>(&self, a: &Agent<'gc, '_>, h: Handle) -> Result<JsObject<'gc>, Status> {
        a.to_object(self.value(h)?).map_err(|_| Status::ObjectExpected)
    }

    /// 句柄里的字符串
    pub fn string<'gc>(&self, h: Handle) -> Result<JsString<'gc>, Status> {
        match self.value(h)?.kind() {
            ValueKind::String(s) => Ok(s),
            _ => Err(Status::StringExpected),
        }
    }

    /// 句柄里的数
    pub fn number(&self, h: Handle) -> Result<f64, Status> {
        self.value(h)?.as_number().ok_or(Status::NumberExpected)
    }

    /// 句柄里的函数
    pub fn callable<'gc>(&self, a: &Agent<'gc, '_>, h: Handle) -> Result<Value<'gc>, Status> {
        let v = self.value(h)?;
        if a.is_callable(v) { Ok(v) } else { Err(Status::FunctionExpected) }
    }

    /// 句柄里的属性名（字符串或 Symbol，不转换）
    pub fn name<'gc>(&self, h: Handle) -> Result<Name<'gc>, Status> {
        match self.value(h)?.kind() {
            ValueKind::String(s) => Ok(Name::Str(s)),
            ValueKind::Symbol(s) => Ok(Name::Sym(s)),
            _ => Err(Status::NameExpected),
        }
    }
}
