//! Conversion types for host functions beyond plain values: [`Persistent`] keeps a JS value across `enter` calls (a
//! host struct can store it), [`Function`] is a callable one, [`Bytes`] is binary data (`ArrayBuffer` / typed arrays /
//! `DataView` ⇄ `Vec<u8>`).
use alloc::boxed::Box;
#[cfg(feature = "json")]
use alloc::string::String;
use alloc::vec::Vec;

use j8framework::{Agent, BufferData, ElementKind, Error, ErrorKind, Global, ObjectData, ObjectKind, Value, ViewData};

use crate::{FromJs, IntoJs};

/// A JS value held across `enter` calls (a GC root; dropping it releases the value). Storing one in a host object that
/// the value itself reaches keeps both alive until the host object is dropped from Rust.
#[derive(Debug)]
pub struct Persistent(pub Global);

impl<'gc> FromJs<'gc> for Persistent {
    fn from_js(a: &mut Agent<'gc, '_>, v: Value<'gc>) -> Result<Self, Error> {
        Ok(Self(a.global(v)))
    }
}

impl<'gc> IntoJs<'gc> for &Persistent {
    fn into_js(self, a: &mut Agent<'gc, '_>) -> Result<Value<'gc>, Error> {
        Ok(a.local(&self.0))
    }
}

impl Persistent {
    /// The value in this entry
    pub fn get<'gc>(&self, a: &Agent<'gc, '_>) -> Value<'gc> {
        a.local(&self.0)
    }
}

/// A JS function held across `enter` calls (a [`Persistent`] that was callable when taken).
#[derive(Debug)]
pub struct Function(pub Persistent);

impl<'gc> FromJs<'gc> for Function {
    fn from_js(a: &mut Agent<'gc, '_>, v: Value<'gc>) -> Result<Self, Error> {
        if !a.is_callable(v) {
            let t = a.describe(v);
            return Err(a.throw(ErrorKind::Type, &alloc::format!("{t} is not a function")));
        }
        Ok(Self(Persistent(a.global(v))))
    }
}

impl<'gc> IntoJs<'gc> for &Function {
    fn into_js(self, a: &mut Agent<'gc, '_>) -> Result<Value<'gc>, Error> {
        Ok(a.local(&self.0.0))
    }
}

impl Function {
    /// Calls it with `this` and `args`
    pub fn call<'gc>(&self, a: &mut Agent<'gc, '_>, this: Value<'gc>, args: &[Value<'gc>]) -> Result<Value<'gc>, Error> {
        let f = a.local(&self.0.0);
        a.call(f, this, args)
    }
}

/// Binary data: taken from an `ArrayBuffer`, a typed array or a `DataView` (the bytes it views), given back as a
/// `Uint8Array`.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Bytes(pub Vec<u8>);

impl<'gc> FromJs<'gc> for Bytes {
    fn from_js(a: &mut Agent<'gc, '_>, v: Value<'gc>) -> Result<Self, Error> {
        let bytes = v.as_object().and_then(|o| {
            let d = o.borrow();
            match &d.kind {
                ObjectKind::ArrayBuffer(b) => b.bytes.clone(),
                ObjectKind::TypedArray(k, view) => Self::view(view, k.size()),
                ObjectKind::DataView(view) => Self::view(view, 1),
                _ => None,
            }
        });
        bytes.map(Self).ok_or_else(|| {
            let t = a.describe(v);
            a.throw(ErrorKind::Type, &alloc::format!("{t} is not an ArrayBuffer, typed array or DataView"))
        })
    }
}

impl Bytes {
    /// The bytes a view covers (`None` when its buffer is detached)
    fn view(view: &ViewData<'_>, size: usize) -> Option<Vec<u8>> {
        let b = view.buffer.borrow();
        let ObjectKind::ArrayBuffer(data) = &b.kind else { return None };
        let bytes = data.bytes.as_ref()?;
        let end = view.length.map_or(bytes.len(), |n| view.offset + n * size).min(bytes.len());
        bytes.get(view.offset..end).map(<[u8]>::to_vec)
    }
}

impl<'gc> IntoJs<'gc> for Bytes {
    fn into_js(self, a: &mut Agent<'gc, '_>) -> Result<Value<'gc>, Error> {
        let len = self.0.len();
        let data = BufferData { bytes: Some(self.0), max: None, shared: false };
        let buffer = ObjectData { kind: ObjectKind::ArrayBuffer(Box::new(data)), proto: Some(a.realm.array_buffer_proto), ..ObjectData::default() }.alloc(a.mc);
        let at = ElementKind::ALL.iter().position(|&k| k == ElementKind::Uint8).expect("Uint8 is a typed array kind");
        let view = ViewData { buffer, offset: 0, length: Some(len) };
        let o = ObjectData { kind: ObjectKind::TypedArray(ElementKind::Uint8, view), proto: Some(a.realm.typed_protos[at]), ..ObjectData::default() }.alloc(a.mc);
        Ok(o.into())
    }
}

/// A Rust value that crosses as JSON-shaped data (`serde`, feature `json`): taken through `JSON.stringify`, given
/// back through `JSON.parse`, so it follows JSON's rules (no functions, `undefined` fields dropped, …).
#[cfg(feature = "json")]
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Json<T>(pub T);

#[cfg(feature = "json")]
impl<T> Json<T> {
    /// Calls `JSON.<name>(arg)`
    fn json<'gc>(a: &mut Agent<'gc, '_>, name: &str, arg: Value<'gc>) -> Result<Value<'gc>, Error> {
        let json = a.get_named(a.realm.global.into(), "JSON")?;
        let f = a.get_named(json, name)?;
        a.call(f, json, &[arg])
    }
}

#[cfg(feature = "json")]
impl<'gc, T: serde::de::DeserializeOwned> FromJs<'gc> for Json<T> {
    fn from_js(a: &mut Agent<'gc, '_>, v: Value<'gc>) -> Result<Self, Error> {
        let text = Self::json(a, "stringify", v)?;
        let text = String::from(&**a.to_string(text)?);
        serde_json::from_str(&text).map(Self).map_err(|e| a.throw(ErrorKind::Type, &alloc::format!("cannot read this value: {e}")))
    }
}

#[cfg(feature = "json")]
impl<'gc, T: serde::Serialize> IntoJs<'gc> for Json<T> {
    fn into_js(self, a: &mut Agent<'gc, '_>) -> Result<Value<'gc>, Error> {
        let text = serde_json::to_string(&self.0).map_err(|e| a.throw(ErrorKind::Type, &alloc::format!("cannot write this value: {e}")))?;
        let s = a.string(&text);
        Self::json(a, "parse", s.into())
    }
}
