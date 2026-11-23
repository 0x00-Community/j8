use alloc::boxed::Box;
use alloc::sync::Arc;

use j8framework::{Scope, Backing, BufferData, Error, ErrorKind, JsObject, ObjectData, ObjectKind, SharedMemory, Value};

use crate::{FromJs, IntoJs};

#[derive(Debug)]
pub struct ArrayBuffer(pub Backing);

impl<'scope> IntoJs<'scope> for ArrayBuffer {
    fn into_js(self, a: &mut Scope<'scope, '_>) -> Result<Value<'scope>, Error> {
        let max = self.0.shared().and_then(|s| s.max());
        let shared = self.0.shared().is_some();
        let proto = if shared { a.context.shared_array_buffer_proto } else { a.context.array_buffer_proto };
        let data = BufferData { bytes: Some(self.0), max, shared };
        Ok(ObjectData { kind: ObjectKind::ArrayBuffer(Box::new(data)), proto: Some(proto), ..ObjectData::default() }.alloc(a.mc).into())
    }
}

#[derive(Clone, Debug)]
pub struct Shared(pub Arc<SharedMemory>);

impl<'scope> FromJs<'scope> for Shared {
    fn from_js(a: &mut Scope<'scope, '_>, v: Value<'scope>) -> Result<Self, Error> {
        let memory = BufferRef::target(v).and_then(|(buffer, ..)| match &buffer.borrow().kind {
            ObjectKind::ArrayBuffer(b) => b.bytes.as_ref().and_then(Backing::shared).cloned(),
            _ => None,
        });
        memory.map(Self).ok_or_else(|| {
            let t = a.describe(v);
            a.throw(ErrorKind::Type, &alloc::format!("{t} is not a SharedArrayBuffer or a view of one"))
        })
    }
}

impl<'scope> IntoJs<'scope> for Shared {
    fn into_js(self, a: &mut Scope<'scope, '_>) -> Result<Value<'scope>, Error> {
        ArrayBuffer(Backing::Shared(self.0)).into_js(a)
    }
}

#[derive(Debug)]
pub struct Transfer(pub Backing);

impl<'scope> FromJs<'scope> for Transfer {
    fn from_js(a: &mut Scope<'scope, '_>, v: Value<'scope>) -> Result<Self, Error> {
        let bytes = v.as_object().and_then(|o| match &mut o.borrow_mut(a.mc).kind {
            ObjectKind::ArrayBuffer(b) if !b.shared => b.bytes.take(),
            _ => None,
        });
        bytes.map(Self).ok_or_else(|| {
            let t = a.describe(v);
            a.throw(ErrorKind::Type, &alloc::format!("{t} is not an attached, non-shared ArrayBuffer"))
        })
    }
}

#[derive(Clone, Copy, Debug)]
pub struct BufferRef<'scope> {
    buffer: JsObject<'scope>,
    offset: usize,
    len: Option<usize>,
}

impl<'scope> FromJs<'scope> for BufferRef<'scope> {
    fn from_js(a: &mut Scope<'scope, '_>, v: Value<'scope>) -> Result<Self, Error> {
        Self::target(v).map(|(buffer, offset, len)| Self { buffer, offset, len }).ok_or_else(|| {
            let t = a.describe(v);
            a.throw(ErrorKind::Type, &alloc::format!("{t} is not an ArrayBuffer, typed array or DataView"))
        })
    }
}

impl<'scope> BufferRef<'scope> {
    fn target(v: Value<'scope>) -> Option<(JsObject<'scope>, usize, Option<usize>)> {
        let o = v.as_object()?;
        let d = o.borrow();
        match &d.kind {
            ObjectKind::ArrayBuffer(_) => Some((o, 0, None)),
            ObjectKind::TypedArray(k, view) => Some((view.buffer, view.offset, view.length.map(|n| n * k.size()))),
            ObjectKind::DataView(view) => Some((view.buffer, view.offset, view.length)),
            _ => None,
        }
    }

    pub fn buffer(&self) -> JsObject<'scope> {
        self.buffer
    }

    pub fn with<R>(&self, a: &Scope<'scope, '_>, f: impl FnOnce(&mut [u8]) -> R) -> Option<R> {
        let ObjectKind::ArrayBuffer(b) = &mut self.buffer.borrow_mut(a.mc).kind else { return None };
        let bytes = b.bytes.as_deref_mut()?;
        let end = self.len.map_or(bytes.len(), |n| self.offset + n);
        bytes.get_mut(self.offset..end).map(f)
    }

    pub fn detach(&self, a: &Scope<'scope, '_>) -> Option<Backing> {
        match &mut self.buffer.borrow_mut(a.mc).kind {
            ObjectKind::ArrayBuffer(b) if !b.shared => b.bytes.take(),
            _ => None,
        }
    }
}
