use j8framework::ValueKind;
use alloc::string::String;
use alloc::vec::Vec;

use j8framework::{Scope, Error, ErrorKind, JsObject, JsString, Value};

pub trait FromJs<'scope>: Sized {
    fn from_js(a: &mut Scope<'scope, '_>, v: Value<'scope>) -> Result<Self, Error>;
}

pub trait IntoJs<'scope> {
    fn into_js(self, a: &mut Scope<'scope, '_>) -> Result<Value<'scope>, Error>;
}

#[derive(Clone, Copy, Debug)]
pub struct This<'scope>(pub Value<'scope>);

#[derive(Clone, Debug)]
pub struct Rest<'scope>(pub Vec<Value<'scope>>);

impl<'scope> FromJs<'scope> for Value<'scope> {
    fn from_js(_: &mut Scope<'scope, '_>, v: Value<'scope>) -> Result<Self, Error> {
        Ok(v)
    }
}

impl<'scope> FromJs<'scope> for f64 {
    fn from_js(a: &mut Scope<'scope, '_>, v: Value<'scope>) -> Result<Self, Error> {
        a.to_number(v)
    }
}

impl<'scope> FromJs<'scope> for i32 {
    fn from_js(a: &mut Scope<'scope, '_>, v: Value<'scope>) -> Result<Self, Error> {
        a.to_int32(v)
    }
}

impl<'scope> FromJs<'scope> for u32 {
    fn from_js(a: &mut Scope<'scope, '_>, v: Value<'scope>) -> Result<Self, Error> {
        a.to_uint32(v)
    }
}

impl<'scope> FromJs<'scope> for usize {
    fn from_js(a: &mut Scope<'scope, '_>, v: Value<'scope>) -> Result<Self, Error> {
        a.to_index(v)
    }
}

impl<'scope> FromJs<'scope> for bool {
    fn from_js(a: &mut Scope<'scope, '_>, v: Value<'scope>) -> Result<Self, Error> {
        Ok(a.to_boolean(v))
    }
}

impl<'scope> FromJs<'scope> for String {
    fn from_js(a: &mut Scope<'scope, '_>, v: Value<'scope>) -> Result<Self, Error> {
        Ok(String::from(&**a.to_string(v)?))
    }
}

impl<'scope> FromJs<'scope> for JsString<'scope> {
    fn from_js(a: &mut Scope<'scope, '_>, v: Value<'scope>) -> Result<Self, Error> {
        a.to_string(v)
    }
}

impl<'scope> FromJs<'scope> for JsObject<'scope> {
    fn from_js(a: &mut Scope<'scope, '_>, v: Value<'scope>) -> Result<Self, Error> {
        match v.kind() {
            ValueKind::Object(o) => Ok(o),
            other => {
                let t = a.describe(Value::from(other));
                Err(a.throw(ErrorKind::Type, &alloc::format!("{t} is not an object")))
            }
        }
    }
}

impl<'scope, T: FromJs<'scope>> FromJs<'scope> for Option<T> {
    fn from_js(a: &mut Scope<'scope, '_>, v: Value<'scope>) -> Result<Self, Error> {
        if v.is_nullish() { Ok(None) } else { T::from_js(a, v).map(Some) }
    }
}

impl<'scope, T: FromJs<'scope>> FromJs<'scope> for Vec<T> {
    fn from_js(a: &mut Scope<'scope, '_>, v: Value<'scope>) -> Result<Self, Error> {
        a.to_list(v)?.into_iter().map(|x| T::from_js(a, x)).collect()
    }
}

impl<'scope> IntoJs<'scope> for Value<'scope> {
    fn into_js(self, _: &mut Scope<'scope, '_>) -> Result<Value<'scope>, Error> {
        Ok(self)
    }
}

impl<'scope> IntoJs<'scope> for () {
    fn into_js(self, _: &mut Scope<'scope, '_>) -> Result<Value<'scope>, Error> {
        Ok(Value::Undefined)
    }
}

impl<'scope> IntoJs<'scope> for f64 {
    fn into_js(self, _: &mut Scope<'scope, '_>) -> Result<Value<'scope>, Error> {
        Ok(self.into())
    }
}

impl<'scope> IntoJs<'scope> for i32 {
    fn into_js(self, _: &mut Scope<'scope, '_>) -> Result<Value<'scope>, Error> {
        Ok(f64::from(self).into())
    }
}

impl<'scope> IntoJs<'scope> for u32 {
    fn into_js(self, _: &mut Scope<'scope, '_>) -> Result<Value<'scope>, Error> {
        Ok(f64::from(self).into())
    }
}

impl<'scope> IntoJs<'scope> for usize {
    fn into_js(self, _: &mut Scope<'scope, '_>) -> Result<Value<'scope>, Error> {
        #[expect(clippy::cast_precision_loss, reason = "lengths / indices are far below 2^53")]
        let x = self as f64;
        Ok(x.into())
    }
}

impl<'scope> IntoJs<'scope> for bool {
    fn into_js(self, _: &mut Scope<'scope, '_>) -> Result<Value<'scope>, Error> {
        Ok(self.into())
    }
}

impl<'scope> IntoJs<'scope> for String {
    fn into_js(self, a: &mut Scope<'scope, '_>) -> Result<Value<'scope>, Error> {
        Ok(a.string(&self).into())
    }
}

impl<'scope> IntoJs<'scope> for &str {
    fn into_js(self, a: &mut Scope<'scope, '_>) -> Result<Value<'scope>, Error> {
        Ok(a.string(self).into())
    }
}

impl<'scope> IntoJs<'scope> for JsString<'scope> {
    fn into_js(self, _: &mut Scope<'scope, '_>) -> Result<Value<'scope>, Error> {
        Ok(self.into())
    }
}

impl<'scope> IntoJs<'scope> for JsObject<'scope> {
    fn into_js(self, _: &mut Scope<'scope, '_>) -> Result<Value<'scope>, Error> {
        Ok(self.into())
    }
}

impl<'scope, T: IntoJs<'scope>> IntoJs<'scope> for Option<T> {
    fn into_js(self, a: &mut Scope<'scope, '_>) -> Result<Value<'scope>, Error> {
        match self {
            Some(v) => v.into_js(a),
            None => Ok(Value::Undefined),
        }
    }
}

impl<'scope, T: IntoJs<'scope>> IntoJs<'scope> for Vec<T> {
    fn into_js(self, a: &mut Scope<'scope, '_>) -> Result<Value<'scope>, Error> {
        let items = self.into_iter().map(|x| x.into_js(a)).collect::<Result<Vec<_>, _>>()?;
        Ok(a.array(items).into())
    }
}

impl<'scope, T: IntoJs<'scope>> IntoJs<'scope> for Result<T, Error> {
    fn into_js(self, a: &mut Scope<'scope, '_>) -> Result<Value<'scope>, Error> {
        match self {
            Ok(v) => v.into_js(a),
            Err(Error::Parked(ticket)) => Err(a.unpark(ticket)),
            Err(e) => Err(e),
        }
    }
}

fn object_of<'scope, T: IntoJs<'scope>>(a: &mut Scope<'scope, '_>, entries: impl IntoIterator<Item = (String, T)>) -> Result<Value<'scope>, Error> {
    let o = a.object();
    for (k, v) in entries {
        let v = v.into_js(a)?;
        o.borrow_mut(a.mc).define(&k, j8framework::Property::data(v));
    }
    Ok(o.into())
}

impl<'scope, T: IntoJs<'scope>> IntoJs<'scope> for alloc::collections::BTreeMap<String, T> {
    fn into_js(self, a: &mut Scope<'scope, '_>) -> Result<Value<'scope>, Error> {
        object_of(a, self)
    }
}

impl<'scope, T: IntoJs<'scope>, S> IntoJs<'scope> for std::collections::HashMap<String, T, S> {
    fn into_js(self, a: &mut Scope<'scope, '_>) -> Result<Value<'scope>, Error> {
        object_of(a, self)
    }
}
