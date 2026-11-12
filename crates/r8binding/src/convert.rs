//! JS 值 ⇄ Rust 值。
use alloc::string::String;
use alloc::vec::Vec;

use r8framework::{Agent, Error, ErrorKind, JsObject, JsString, Value};

/// JS 值 ⇒ Rust 值（宿主函数的实参）。
pub trait FromJs<'gc>: Sized {
    fn from_js(a: &mut Agent<'gc, '_>, v: Value<'gc>) -> Result<Self, Error>;
}

/// Rust 值 ⇒ JS 值（宿主函数的返回值）。
pub trait IntoJs<'gc> {
    fn into_js(self, a: &mut Agent<'gc, '_>) -> Result<Value<'gc>, Error>;
}

/// 方法里要原样的 `this`。
#[derive(Clone, Copy, Debug)]
pub struct This<'gc>(pub Value<'gc>);

/// 剩下的全部实参（放在最后一个形参）。
#[derive(Clone, Debug)]
pub struct Rest<'gc>(pub Vec<Value<'gc>>);

impl<'gc> FromJs<'gc> for Value<'gc> {
    fn from_js(_: &mut Agent<'gc, '_>, v: Value<'gc>) -> Result<Self, Error> {
        Ok(v)
    }
}

impl<'gc> FromJs<'gc> for f64 {
    fn from_js(a: &mut Agent<'gc, '_>, v: Value<'gc>) -> Result<Self, Error> {
        a.to_number(v)
    }
}

impl<'gc> FromJs<'gc> for i32 {
    fn from_js(a: &mut Agent<'gc, '_>, v: Value<'gc>) -> Result<Self, Error> {
        a.to_int32(v)
    }
}

impl<'gc> FromJs<'gc> for u32 {
    fn from_js(a: &mut Agent<'gc, '_>, v: Value<'gc>) -> Result<Self, Error> {
        a.to_uint32(v)
    }
}

/// 下标 / 长度：ToIndex。
impl<'gc> FromJs<'gc> for usize {
    fn from_js(a: &mut Agent<'gc, '_>, v: Value<'gc>) -> Result<Self, Error> {
        a.to_index(v)
    }
}

impl<'gc> FromJs<'gc> for bool {
    fn from_js(a: &mut Agent<'gc, '_>, v: Value<'gc>) -> Result<Self, Error> {
        Ok(a.to_boolean(v))
    }
}

impl<'gc> FromJs<'gc> for String {
    fn from_js(a: &mut Agent<'gc, '_>, v: Value<'gc>) -> Result<Self, Error> {
        Ok(String::from(&**a.to_string(v)?))
    }
}

impl<'gc> FromJs<'gc> for JsString<'gc> {
    fn from_js(a: &mut Agent<'gc, '_>, v: Value<'gc>) -> Result<Self, Error> {
        a.to_string(v)
    }
}

impl<'gc> FromJs<'gc> for JsObject<'gc> {
    fn from_js(a: &mut Agent<'gc, '_>, v: Value<'gc>) -> Result<Self, Error> {
        match v {
            Value::Object(o) => Ok(o),
            other => {
                let t = a.describe(other);
                Err(a.throw(ErrorKind::Type, &alloc::format!("{t} is not an object")))
            }
        }
    }
}

/// `undefined` / `null` 是 `None`。
impl<'gc, T: FromJs<'gc>> FromJs<'gc> for Option<T> {
    fn from_js(a: &mut Agent<'gc, '_>, v: Value<'gc>) -> Result<Self, Error> {
        if v.is_nullish() { Ok(None) } else { T::from_js(a, v).map(Some) }
    }
}

/// 可迭代对象逐项转。
impl<'gc, T: FromJs<'gc>> FromJs<'gc> for Vec<T> {
    fn from_js(a: &mut Agent<'gc, '_>, v: Value<'gc>) -> Result<Self, Error> {
        a.to_list(v)?.into_iter().map(|x| T::from_js(a, x)).collect()
    }
}

impl<'gc> IntoJs<'gc> for Value<'gc> {
    fn into_js(self, _: &mut Agent<'gc, '_>) -> Result<Value<'gc>, Error> {
        Ok(self)
    }
}

impl<'gc> IntoJs<'gc> for () {
    fn into_js(self, _: &mut Agent<'gc, '_>) -> Result<Value<'gc>, Error> {
        Ok(Value::Undefined)
    }
}

impl<'gc> IntoJs<'gc> for f64 {
    fn into_js(self, _: &mut Agent<'gc, '_>) -> Result<Value<'gc>, Error> {
        Ok(self.into())
    }
}

impl<'gc> IntoJs<'gc> for i32 {
    fn into_js(self, _: &mut Agent<'gc, '_>) -> Result<Value<'gc>, Error> {
        Ok(f64::from(self).into())
    }
}

impl<'gc> IntoJs<'gc> for u32 {
    fn into_js(self, _: &mut Agent<'gc, '_>) -> Result<Value<'gc>, Error> {
        Ok(f64::from(self).into())
    }
}

impl<'gc> IntoJs<'gc> for usize {
    fn into_js(self, _: &mut Agent<'gc, '_>) -> Result<Value<'gc>, Error> {
        #[expect(clippy::cast_precision_loss, reason = "长度 / 下标远小于 2^53")]
        let x = self as f64;
        Ok(x.into())
    }
}

impl<'gc> IntoJs<'gc> for bool {
    fn into_js(self, _: &mut Agent<'gc, '_>) -> Result<Value<'gc>, Error> {
        Ok(self.into())
    }
}

impl<'gc> IntoJs<'gc> for String {
    fn into_js(self, a: &mut Agent<'gc, '_>) -> Result<Value<'gc>, Error> {
        Ok(a.string(&self).into())
    }
}

impl<'gc> IntoJs<'gc> for &str {
    fn into_js(self, a: &mut Agent<'gc, '_>) -> Result<Value<'gc>, Error> {
        Ok(a.string(self).into())
    }
}

impl<'gc> IntoJs<'gc> for JsString<'gc> {
    fn into_js(self, _: &mut Agent<'gc, '_>) -> Result<Value<'gc>, Error> {
        Ok(self.into())
    }
}

impl<'gc> IntoJs<'gc> for JsObject<'gc> {
    fn into_js(self, _: &mut Agent<'gc, '_>) -> Result<Value<'gc>, Error> {
        Ok(self.into())
    }
}

/// `None` 是 `undefined`。
impl<'gc, T: IntoJs<'gc>> IntoJs<'gc> for Option<T> {
    fn into_js(self, a: &mut Agent<'gc, '_>) -> Result<Value<'gc>, Error> {
        match self {
            Some(v) => v.into_js(a),
            None => Ok(Value::Undefined),
        }
    }
}

/// 数组。
impl<'gc, T: IntoJs<'gc>> IntoJs<'gc> for Vec<T> {
    fn into_js(self, a: &mut Agent<'gc, '_>) -> Result<Value<'gc>, Error> {
        let items = self.into_iter().map(|x| x.into_js(a)).collect::<Result<Vec<_>, _>>()?;
        Ok(a.array(items).into())
    }
}

/// 出错就抛。
impl<'gc, T: IntoJs<'gc>> IntoJs<'gc> for Result<T, Error> {
    fn into_js(self, a: &mut Agent<'gc, '_>) -> Result<Value<'gc>, Error> {
        self?.into_js(a)
    }
}
