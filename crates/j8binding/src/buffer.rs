//! 不拷贝的二进制数据：[`ArrayBuffer`] 把字节（移进来的 `Vec`、外部或共享内存）交给 JS，[`Shared`] 取 `SharedArrayBuffer`
//! 背后的内存，[`Transfer`] detach 一个 `ArrayBuffer` 并拿走字节，[`BufferRef`] 原地读写 buffer / 视图覆盖的字节。
//! 要拷贝的用 [`Bytes`](crate::Bytes)。
use alloc::boxed::Box;
use alloc::sync::Arc;

use j8framework::{Agent, Backing, BufferData, Error, ErrorKind, JsObject, ObjectData, ObjectKind, SharedMemory, Value};

use crate::{FromJs, IntoJs};

/// 原样交给 JS 的字节：`Vec`（移进来）或外部内存成 `ArrayBuffer`，共享内存成 `SharedArrayBuffer`。
#[derive(Debug)]
pub struct ArrayBuffer(pub Backing);

impl<'gc> IntoJs<'gc> for ArrayBuffer {
    fn into_js(self, a: &mut Agent<'gc, '_>) -> Result<Value<'gc>, Error> {
        let max = self.0.shared().and_then(|s| s.max());
        let shared = self.0.shared().is_some();
        let proto = if shared { a.realm.shared_array_buffer_proto } else { a.realm.array_buffer_proto };
        let data = BufferData { bytes: Some(self.0), max, shared };
        Ok(ObjectData { kind: ObjectKind::ArrayBuffer(Box::new(data)), proto: Some(proto), ..ObjectData::default() }.alloc(a.mc).into())
    }
}

/// `SharedArrayBuffer`（或它的视图）背后的内存：交给另一个 isolate（[`IntoJs`]）后两边看到同一份字节。
#[derive(Clone, Debug)]
pub struct Shared(pub Arc<SharedMemory>);

impl<'gc> FromJs<'gc> for Shared {
    fn from_js(a: &mut Agent<'gc, '_>, v: Value<'gc>) -> Result<Self, Error> {
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

impl<'gc> IntoJs<'gc> for Shared {
    fn into_js(self, a: &mut Agent<'gc, '_>) -> Result<Value<'gc>, Error> {
        ArrayBuffer(Backing::Shared(self.0)).into_js(a)
    }
}

/// 拿走一个 `ArrayBuffer` 的字节，原 buffer 被 detach（同 `transfer()` / `postMessage` 的转移列表）。
#[derive(Debug)]
pub struct Transfer(pub Backing);

impl<'gc> FromJs<'gc> for Transfer {
    fn from_js(a: &mut Agent<'gc, '_>, v: Value<'gc>) -> Result<Self, Error> {
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

/// `ArrayBuffer`、typed array 或 `DataView` 覆盖的字节，原地使用不拷贝；只在取到它的那次调用里有效。
#[derive(Clone, Copy, Debug)]
pub struct BufferRef<'gc> {
    buffer: JsObject<'gc>,
    offset: usize,
    /// 字节长度；`None` 跟着 buffer 长度走
    len: Option<usize>,
}

impl<'gc> FromJs<'gc> for BufferRef<'gc> {
    fn from_js(a: &mut Agent<'gc, '_>, v: Value<'gc>) -> Result<Self, Error> {
        Self::target(v).map(|(buffer, offset, len)| Self { buffer, offset, len }).ok_or_else(|| {
            let t = a.describe(v);
            a.throw(ErrorKind::Type, &alloc::format!("{t} is not an ArrayBuffer, typed array or DataView"))
        })
    }
}

impl<'gc> BufferRef<'gc> {
    /// buffer 或视图的（buffer、字节偏移、字节长度）
    fn target(v: Value<'gc>) -> Option<(JsObject<'gc>, usize, Option<usize>)> {
        let o = v.as_object()?;
        let d = o.borrow();
        match &d.kind {
            ObjectKind::ArrayBuffer(_) => Some((o, 0, None)),
            ObjectKind::TypedArray(k, view) => Some((view.buffer, view.offset, view.length.map(|n| n * k.size()))),
            ObjectKind::DataView(view) => Some((view.buffer, view.offset, view.length)),
            _ => None,
        }
    }

    /// buffer 对象本身
    pub fn buffer(&self) -> JsObject<'gc> {
        self.buffer
    }

    /// 对这些字节跑 `f`；buffer 已 detach 或视图越界时返回 `None`。
    pub fn with<R>(&self, a: &Agent<'gc, '_>, f: impl FnOnce(&mut [u8]) -> R) -> Option<R> {
        let ObjectKind::ArrayBuffer(b) = &mut self.buffer.borrow_mut(a.mc).kind else { return None };
        let bytes = b.bytes.as_deref_mut()?;
        let end = self.len.map_or(bytes.len(), |n| self.offset + n);
        bytes.get_mut(self.offset..end).map(f)
    }

    /// detach 这个 buffer 并拿走字节（整个 buffer，不只是视图那段）；共享的或已 detach 的返回 `None`。
    /// 提前释放外部内存（WebGPU `unmap`）走这里。
    pub fn detach(&self, a: &Agent<'gc, '_>) -> Option<Backing> {
        match &mut self.buffer.borrow_mut(a.mc).kind {
            ObjectKind::ArrayBuffer(b) if !b.shared => b.bytes.take(),
            _ => None,
        }
    }
}
