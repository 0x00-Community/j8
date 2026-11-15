use r8framework::ValueKind;
use alloc::boxed::Box;
use alloc::vec::Vec;

use r8framework::{Agent, BufferData, ElementKind, Error, ErrorKind, JsObject, ObjectKind, Property, Slot, Value, ViewData};

use crate::{Args, Installer};

/// `ArrayBuffer` / `SharedArrayBuffer` / `DataView` / `Atomics`.
pub(crate) struct Buffers;

/// Maximum number of bytes allocated at once; more throws RangeError
const MAX_BYTES: usize = 1 << 31;

/// Atomics read-modify-write operations.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Rmw {
    Add,
    Sub,
    And,
    Or,
    Xor,
    Exchange,
}

impl Buffers {
    /// A new buffer (all bytes zero).
    pub(crate) fn allocate<'gc>(a: &mut Agent<'gc, '_>, proto: JsObject<'gc>, len: usize, max: Option<usize>, shared: bool) -> Result<JsObject<'gc>, Error> {
        if len > MAX_BYTES || max.is_some_and(|m| m > MAX_BYTES) {
            return Err(a.throw(ErrorKind::Range, "Array buffer allocation failed"));
        }
        let data = BufferData { bytes: Some(alloc::vec![0; len]), max, shared };
        Ok(r8framework::ObjectData { kind: ObjectKind::ArrayBuffer(Box::new(data)), proto: Some(proto), ..r8framework::ObjectData::default() }.alloc(a.mc))
    }

    /// `this` is a (shared / non-shared) buffer.
    fn this_buffer<'gc>(a: &Agent<'gc, '_>, this: Value<'gc>, shared: bool) -> Result<JsObject<'gc>, Error> {
        match this.kind() {
            ValueKind::Object(o) if matches!(&o.borrow().kind, ObjectKind::ArrayBuffer(b) if b.shared == shared) => Ok(o),
            _ => Err(a.throw(ErrorKind::Type, if shared { "Method called on a non-SharedArrayBuffer" } else { "Method called on a non-ArrayBuffer" })),
        }
    }

    fn info(o: JsObject<'_>) -> (Option<usize>, Option<usize>) {
        match &o.borrow().kind {
            ObjectKind::ArrayBuffer(b) => (b.bytes.as_ref().map(Vec::len), b.max),
            _ => (None, None),
        }
    }

    fn construct_with<'gc>(a: &mut Agent<'gc, '_>, args: &[Value<'gc>], shared: bool) -> Result<Value<'gc>, Error> {
        if a.new_target == Value::Undefined {
            return Err(a.throw(ErrorKind::Type, if shared { "Constructor SharedArrayBuffer requires 'new'" } else { "Constructor ArrayBuffer requires 'new'" }));
        }
        let len = a.to_index(args.at(0))?;
        let max = match args.at(1).kind() {
            o @ ValueKind::Object(_) => match (a.get_named(Value::from(o), "maxByteLength")?).kind() {
                ValueKind::Undefined => None,
                m => Some(a.to_index(Value::from(m))?),
            },
            _ => None,
        };
        if max.is_some_and(|m| len > m) {
            return Err(a.throw(ErrorKind::Range, "byteLength exceeds maxByteLength"));
        }
        let fallback = if shared { a.realm.shared_array_buffer_proto } else { a.realm.array_buffer_proto };
        let proto = a.proto_from_ctor(a.new_target, fallback)?;
        Ok(Self::allocate(a, proto, len, max, shared)?.into())
    }

    fn array_buffer<'gc>(a: &mut Agent<'gc, '_>, _: Value<'gc>, args: &[Value<'gc>]) -> Result<Value<'gc>, Error> {
        Self::construct_with(a, args, false)
    }

    fn shared_array_buffer<'gc>(a: &mut Agent<'gc, '_>, _: Value<'gc>, args: &[Value<'gc>]) -> Result<Value<'gc>, Error> {
        Self::construct_with(a, args, true)
    }

    fn is_view<'gc>(_: &mut Agent<'gc, '_>, _: Value<'gc>, args: &[Value<'gc>]) -> Result<Value<'gc>, Error> {
        Ok(matches!(args.at(0).kind(), ValueKind::Object(o) if matches!(o.borrow().kind, ObjectKind::TypedArray(..) | ObjectKind::DataView(_))).into())
    }

    fn species<'gc>(_: &mut Agent<'gc, '_>, this: Value<'gc>, _: &[Value<'gc>]) -> Result<Value<'gc>, Error> {
        Ok(this)
    }

    fn byte_length<'gc, const SHARED: bool>(a: &mut Agent<'gc, '_>, this: Value<'gc>, _: &[Value<'gc>]) -> Result<Value<'gc>, Error> {
        let o = Self::this_buffer(a, this, SHARED)?;
        Ok(Self::number(Self::info(o).0.unwrap_or(0)))
    }

    fn number<'gc>(n: usize) -> Value<'gc> {
        #[expect(clippy::cast_precision_loss, reason = "byte count is at most 2^31")]
        let x = n as f64;
        x.into()
    }

    fn max_byte_length<'gc, const SHARED: bool>(a: &mut Agent<'gc, '_>, this: Value<'gc>, _: &[Value<'gc>]) -> Result<Value<'gc>, Error> {
        let o = Self::this_buffer(a, this, SHARED)?;
        let (len, max) = Self::info(o);
        Ok(Self::number(match len {
            None => 0,
            Some(l) => max.unwrap_or(l),
        }))
    }

    fn resizable<'gc, const SHARED: bool>(a: &mut Agent<'gc, '_>, this: Value<'gc>, _: &[Value<'gc>]) -> Result<Value<'gc>, Error> {
        let o = Self::this_buffer(a, this, SHARED)?;
        Ok(Self::info(o).1.is_some().into())
    }

    fn detached<'gc>(a: &mut Agent<'gc, '_>, this: Value<'gc>, _: &[Value<'gc>]) -> Result<Value<'gc>, Error> {
        let o = Self::this_buffer(a, this, false)?;
        Ok(Self::info(o).0.is_none().into())
    }

    fn set_bytes<'gc>(a: &Agent<'gc, '_>, o: JsObject<'gc>, bytes: Option<Vec<u8>>) {
        if let ObjectKind::ArrayBuffer(b) = &mut o.borrow_mut(a.mc).kind {
            b.bytes = bytes;
        }
    }

    fn bytes_of(o: JsObject<'_>) -> Option<Vec<u8>> {
        match &o.borrow().kind {
            ObjectKind::ArrayBuffer(b) => b.bytes.clone(),
            _ => None,
        }
    }

    /// `resize` / `grow`: `grow` cannot shrink.
    fn resize_with<'gc>(a: &mut Agent<'gc, '_>, this: Value<'gc>, args: &[Value<'gc>], shared: bool) -> Result<Value<'gc>, Error> {
        let o = Self::this_buffer(a, this, shared)?;
        let Some(max) = Self::info(o).1 else {
            return Err(a.throw(ErrorKind::Type, "Method called on a fixed-length buffer"));
        };
        let n = a.to_index(args.at(0))?;
        let Some(mut bytes) = Self::bytes_of(o) else {
            return Err(a.throw(ErrorKind::Type, "Cannot resize a detached ArrayBuffer"));
        };
        if n > max || (shared && n < bytes.len()) {
            return Err(a.throw(ErrorKind::Range, "Invalid length for resize"));
        }
        bytes.resize(n, 0);
        Self::set_bytes(a, o, Some(bytes));
        Ok(Value::Undefined)
    }

    fn resize<'gc>(a: &mut Agent<'gc, '_>, this: Value<'gc>, args: &[Value<'gc>]) -> Result<Value<'gc>, Error> {
        Self::resize_with(a, this, args, false)
    }

    fn grow<'gc>(a: &mut Agent<'gc, '_>, this: Value<'gc>, args: &[Value<'gc>]) -> Result<Value<'gc>, Error> {
        Self::resize_with(a, this, args, true)
    }

    /// SpeciesConstructor(o, fallback).
    pub(crate) fn species_constructor<'gc>(a: &mut Agent<'gc, '_>, o: Value<'gc>, fallback: Value<'gc>) -> Result<Value<'gc>, Error> {
        let c = a.get_named(o, "constructor")?;
        if c == Value::Undefined {
            return Ok(fallback);
        }
        if !matches!(c.kind(), ValueKind::Object(_)) {
            return Err(a.throw(ErrorKind::Type, "object.constructor is not an object"));
        }
        let s = a.get_symbol(c, a.realm.symbols.species, c)?;
        if s.is_nullish() {
            return Ok(fallback);
        }
        if a.is_constructor(s) {
            return Ok(s);
        }
        Err(a.throw(ErrorKind::Type, "object.constructor[Symbol.species] is not a constructor"))
    }

    fn global_ctor<'gc>(a: &Agent<'gc, '_>, name: &str) -> Value<'gc> {
        a.realm.global.borrow().own(name).unwrap_or_default()
    }

    fn slice_with<'gc>(a: &mut Agent<'gc, '_>, this: Value<'gc>, args: &[Value<'gc>], shared: bool) -> Result<Value<'gc>, Error> {
        let o = Self::this_buffer(a, this, shared)?;
        let Some(len) = Self::info(o).0 else {
            return Err(a.throw(ErrorKind::Type, "Cannot slice a detached ArrayBuffer"));
        };
        #[expect(clippy::cast_precision_loss, reason = "byte count is at most 2^31")]
        let lenf = len as f64;
        let rel = |a: &mut Agent<'gc, '_>, v: Value<'gc>, default: f64| -> Result<f64, Error> {
            if v == Value::Undefined {
                return Ok(default);
            }
            let x = a.to_integer_or_infinity(v)?;
            Ok(if x < 0.0 { (lenf + x).max(0.0) } else { x.min(lenf) })
        };
        let first = rel(a, args.at(0), 0.0)?;
        let last = rel(a, args.at(1), lenf)?;
        let new_len = (last - first).max(0.0);
        let fallback = Self::global_ctor(a, if shared { "SharedArrayBuffer" } else { "ArrayBuffer" });
        let ctor = Self::species_constructor(a, o.into(), fallback)?;
        let made = a.construct(ctor, &[new_len.into()], ctor)?;
        let ValueKind::Object(n) = made.kind() else { return Err(a.throw(ErrorKind::Type, "species constructor returned a non-object")) };
        let ok = matches!(&n.borrow().kind, ObjectKind::ArrayBuffer(b) if b.shared == shared && b.bytes.is_some());
        if !ok {
            return Err(a.throw(ErrorKind::Type, "species constructor did not return a usable buffer"));
        }
        if r8gc::Gc::ptr_eq(n, o) {
            return Err(a.throw(ErrorKind::Type, "species constructor returned the same buffer"));
        }
        #[expect(clippy::cast_possible_truncation, clippy::cast_sign_loss, reason = "clamped to [0, len]")]
        let (first, new_len) = (first as usize, new_len as usize);
        if Self::info(n).0.unwrap_or(0) < new_len {
            return Err(a.throw(ErrorKind::Type, "species constructor returned a too small buffer"));
        }
        let Some(src) = Self::bytes_of(o) else {
            return Err(a.throw(ErrorKind::Type, "ArrayBuffer was detached"));
        };
        let end = (first + new_len).min(src.len());
        if first < end
            && let ObjectKind::ArrayBuffer(b) = &mut n.borrow_mut(a.mc).kind
            && let Some(dst) = b.bytes.as_mut()
        {
            dst[..end - first].copy_from_slice(&src[first..end]);
        }
        Ok(n.into())
    }

    fn slice<'gc>(a: &mut Agent<'gc, '_>, this: Value<'gc>, args: &[Value<'gc>]) -> Result<Value<'gc>, Error> {
        Self::slice_with(a, this, args, false)
    }

    fn shared_slice<'gc>(a: &mut Agent<'gc, '_>, this: Value<'gc>, args: &[Value<'gc>]) -> Result<Value<'gc>, Error> {
        Self::slice_with(a, this, args, true)
    }

    /// `transfer` / `transferToFixedLength`: a new buffer takes over the bytes and the original is detached.
    fn transfer_with<'gc>(a: &mut Agent<'gc, '_>, this: Value<'gc>, args: &[Value<'gc>], keep_resizable: bool) -> Result<Value<'gc>, Error> {
        let o = Self::this_buffer(a, this, false)?;
        let new_len = match args.at(0).kind() {
            ValueKind::Undefined => Self::info(o).0.unwrap_or(0),
            v => a.to_index(Value::from(v))?,
        };
        let (len, max) = Self::info(o);
        let Some(_) = len else {
            return Err(a.throw(ErrorKind::Type, "Cannot transfer a detached ArrayBuffer"));
        };
        let max = if keep_resizable { max } else { None };
        if max.is_some_and(|m| new_len > m) {
            return Err(a.throw(ErrorKind::Range, "new length exceeds maxByteLength"));
        }
        let n = Self::allocate(a, a.realm.array_buffer_proto, new_len, max, false)?;
        let mut bytes = Self::bytes_of(o).unwrap_or_default();
        bytes.resize(new_len, 0);
        Self::set_bytes(a, n, Some(bytes));
        Self::set_bytes(a, o, None);
        Ok(n.into())
    }

    fn transfer<'gc>(a: &mut Agent<'gc, '_>, this: Value<'gc>, args: &[Value<'gc>]) -> Result<Value<'gc>, Error> {
        Self::transfer_with(a, this, args, true)
    }

    fn transfer_fixed<'gc>(a: &mut Agent<'gc, '_>, this: Value<'gc>, args: &[Value<'gc>]) -> Result<Value<'gc>, Error> {
        Self::transfer_with(a, this, args, false)
    }

    // ---- DataView ----

    fn data_view<'gc>(a: &mut Agent<'gc, '_>, _: Value<'gc>, args: &[Value<'gc>]) -> Result<Value<'gc>, Error> {
        if a.new_target == Value::Undefined {
            return Err(a.throw(ErrorKind::Type, "Constructor DataView requires 'new'"));
        }
        let ValueKind::Object(buffer) = args.at(0).kind() else { return Err(a.throw(ErrorKind::Type, "First argument to DataView constructor must be an ArrayBuffer")) };
        if !matches!(buffer.borrow().kind, ObjectKind::ArrayBuffer(_)) {
            return Err(a.throw(ErrorKind::Type, "First argument to DataView constructor must be an ArrayBuffer"));
        }
        let offset = a.to_index(args.at(1))?;
        let (Some(len), max) = Self::info(buffer) else { return Err(a.throw(ErrorKind::Type, "Cannot construct a DataView on a detached ArrayBuffer")) };
        if offset > len {
            return Err(a.throw(ErrorKind::Range, "Start offset is outside the bounds of the buffer"));
        }
        let length = match args.at(2).kind() {
            ValueKind::Undefined if max.is_some() => None,
            ValueKind::Undefined => Some(len - offset),
            v => {
                let n = a.to_index(Value::from(v))?;
                if offset + n > len {
                    return Err(a.throw(ErrorKind::Range, "Invalid DataView length"));
                }
                Some(n)
            }
        };
        let proto = a.proto_from_ctor(a.new_target, a.realm.data_view_proto)?;
        // Getting the prototype may have run user code that changed the buffer: check again
        let Some(len) = Self::info(buffer).0 else { return Err(a.throw(ErrorKind::Type, "Cannot construct a DataView on a detached ArrayBuffer")) };
        if offset > len || length.is_some_and(|n| offset + n > len) {
            return Err(a.throw(ErrorKind::Range, "Invalid DataView length"));
        }
        let view = ViewData { buffer, offset, length };
        Ok(r8framework::ObjectData { kind: ObjectKind::DataView(view), proto: Some(proto), ..r8framework::ObjectData::default() }.alloc(a.mc).into())
    }

    fn this_view<'gc>(a: &Agent<'gc, '_>, this: Value<'gc>) -> Result<ViewData<'gc>, Error> {
        match this.kind() {
            ValueKind::Object(o) => match o.borrow().kind {
                ObjectKind::DataView(v) => Ok(v),
                _ => Err(a.throw(ErrorKind::Type, "Method called on a non-DataView")),
            },
            _ => Err(a.throw(ErrorKind::Type, "Method called on a non-DataView")),
        }
    }

    fn view_buffer<'gc>(a: &mut Agent<'gc, '_>, this: Value<'gc>, _: &[Value<'gc>]) -> Result<Value<'gc>, Error> {
        Ok(Self::this_view(a, this)?.buffer.into())
    }

    fn view_byte_length<'gc>(a: &mut Agent<'gc, '_>, this: Value<'gc>, _: &[Value<'gc>]) -> Result<Value<'gc>, Error> {
        let v = Self::this_view(a, this)?;
        match Agent::view_length(&v, 1) {
            Some(n) => Ok(Self::number(n)),
            None => Err(a.throw(ErrorKind::Type, "DataView is out of bounds")),
        }
    }

    fn view_byte_offset<'gc>(a: &mut Agent<'gc, '_>, this: Value<'gc>, _: &[Value<'gc>]) -> Result<Value<'gc>, Error> {
        let v = Self::this_view(a, this)?;
        match Agent::view_length(&v, 1) {
            Some(_) => Ok(Self::number(v.offset)),
            None => Err(a.throw(ErrorKind::Type, "DataView is out of bounds")),
        }
    }

    /// GetViewValue: `K` is the element type index.
    fn get_value<'gc, const K: usize>(a: &mut Agent<'gc, '_>, this: Value<'gc>, args: &[Value<'gc>]) -> Result<Value<'gc>, Error> {
        let kind = ElementKind::ALL[K];
        let v = Self::this_view(a, this)?;
        let index = a.to_index(args.at(0))?;
        let little = a.to_boolean(args.at(1));
        let at = Self::view_access(a, &v, index, kind)?;
        let mut bytes = Self::bytes_of(v.buffer).unwrap_or_default()[at..at + kind.size()].to_vec();
        if !little {
            bytes.reverse();
        }
        Ok(a.element_value(kind.decode(&bytes)))
    }

    /// An element fits starting at byte `index` of the view: returns its position in the buffer.
    fn view_access<'gc>(a: &Agent<'gc, '_>, v: &ViewData<'gc>, index: usize, kind: ElementKind) -> Result<usize, Error> {
        let Some(len) = Agent::view_length(v, 1) else { return Err(a.throw(ErrorKind::Type, "DataView is out of bounds")) };
        if index + kind.size() > len {
            return Err(a.throw(ErrorKind::Range, "Offset is outside the bounds of the DataView"));
        }
        Ok(v.offset + index)
    }

    fn set_value<'gc, const K: usize>(a: &mut Agent<'gc, '_>, this: Value<'gc>, args: &[Value<'gc>]) -> Result<Value<'gc>, Error> {
        let kind = ElementKind::ALL[K];
        let v = Self::this_view(a, this)?;
        let index = a.to_index(args.at(0))?;
        let e = a.to_element(kind, args.at(1))?;
        let little = a.to_boolean(args.at(2));
        let at = Self::view_access(a, &v, index, kind)?;
        let mut bytes = kind.encode(&e);
        if !little {
            bytes.reverse();
        }
        if let ObjectKind::ArrayBuffer(b) = &mut v.buffer.borrow_mut(a.mc).kind
            && let Some(dst) = b.bytes.as_mut()
        {
            dst[at..at + bytes.len()].copy_from_slice(&bytes);
        }
        Ok(Value::Undefined)
    }

    // ---- Atomics ----

    /// ValidateIntegerTypedArray + ValidateAtomicAccess: returns (element type, buffer, byte position).
    fn atomic_target<'gc>(a: &mut Agent<'gc, '_>, ta: Value<'gc>, index: Value<'gc>, waitable: bool) -> Result<(ElementKind, JsObject<'gc>, usize), Error> {
        let (kind, view, len) = Self::integer_array(a, ta, waitable)?;
        let i = a.to_index(index)?;
        if i >= len {
            return Err(a.throw(ErrorKind::Range, "Atomics access index out of range"));
        }
        Ok((kind, view.buffer, view.offset + i * kind.size()))
    }

    /// ValidateIntegerTypedArray: returns (element type, view, length).
    fn integer_array<'gc>(a: &mut Agent<'gc, '_>, ta: Value<'gc>, waitable: bool) -> Result<(ElementKind, ViewData<'gc>, usize), Error> {
        let Some((kind, view)) = (match ta.kind() {
            ValueKind::Object(o) => Agent::typed_array(o),
            _ => None,
        }) else {
            return Err(a.throw(ErrorKind::Type, "Atomics operation on a non-TypedArray"));
        };
        let Some(len) = Agent::view_length(&view, kind.size()) else { return Err(a.throw(ErrorKind::Type, "TypedArray is out of bounds")) };
        let ok = if waitable {
            matches!(kind, ElementKind::Int32 | ElementKind::BigInt64)
        } else {
            !matches!(kind, ElementKind::Uint8Clamped | ElementKind::Float16 | ElementKind::Float32 | ElementKind::Float64)
        };
        if !ok {
            return Err(a.throw(ErrorKind::Type, "Atomics operation on an unsupported TypedArray type"));
        }
        Ok((kind, view, len))
    }

    /// Check again after converting the value to write (the conversion may have detached / shrunk the buffer).
    fn revalidate<'gc>(a: &Agent<'gc, '_>, buffer: JsObject<'gc>, at: usize, kind: ElementKind) -> Result<(), Error> {
        match Self::info(buffer).0 {
            None => Err(a.throw(ErrorKind::Type, "TypedArray is detached")),
            Some(len) if at + kind.size() > len => Err(a.throw(ErrorKind::Range, "Atomics access index out of range")),
            Some(_) => Ok(()),
        }
    }

    /// Numbers for Atomics: ToBigInt for BigInt types, ToIntegerOrInfinity otherwise.
    fn atomic_value<'gc>(a: &mut Agent<'gc, '_>, kind: ElementKind, v: Value<'gc>) -> Result<r8framework::Element, Error> {
        if kind.is_bigint() { Ok(r8framework::Element::BigInt(a.to_bigint(v)?)) } else { Ok(r8framework::Element::Number(a.to_integer_or_infinity(v)?)) }
    }

    fn raw(bytes: &[u8]) -> u64 {
        let mut x = [0u8; 8];
        x[..bytes.len()].copy_from_slice(bytes);
        u64::from_le_bytes(x)
    }

    fn rmw<'gc>(a: &mut Agent<'gc, '_>, args: &[Value<'gc>], op: Rmw) -> Result<Value<'gc>, Error> {
        let (kind, buffer, at) = Self::atomic_target(a, args.at(0), args.at(1), false)?;
        let v = Self::atomic_value(a, kind, args.at(2))?;
        Self::revalidate(a, buffer, at, kind)?;
        let size = kind.size();
        let old = Self::bytes_of(buffer).unwrap_or_default()[at..at + size].to_vec();
        let (x, y) = (Self::raw(&old), Self::raw(&kind.encode(&v)));
        let r = match op {
            Rmw::Add => x.wrapping_add(y),
            Rmw::Sub => x.wrapping_sub(y),
            Rmw::And => x & y,
            Rmw::Or => x | y,
            Rmw::Xor => x ^ y,
            Rmw::Exchange => y,
        };
        if let ObjectKind::ArrayBuffer(b) = &mut buffer.borrow_mut(a.mc).kind
            && let Some(dst) = b.bytes.as_mut()
        {
            dst[at..at + size].copy_from_slice(&r.to_le_bytes()[..size]);
        }
        Ok(a.element_value(kind.decode(&old)))
    }

    fn add<'gc>(a: &mut Agent<'gc, '_>, _: Value<'gc>, args: &[Value<'gc>]) -> Result<Value<'gc>, Error> {
        Self::rmw(a, args, Rmw::Add)
    }

    fn sub<'gc>(a: &mut Agent<'gc, '_>, _: Value<'gc>, args: &[Value<'gc>]) -> Result<Value<'gc>, Error> {
        Self::rmw(a, args, Rmw::Sub)
    }

    fn and<'gc>(a: &mut Agent<'gc, '_>, _: Value<'gc>, args: &[Value<'gc>]) -> Result<Value<'gc>, Error> {
        Self::rmw(a, args, Rmw::And)
    }

    fn or<'gc>(a: &mut Agent<'gc, '_>, _: Value<'gc>, args: &[Value<'gc>]) -> Result<Value<'gc>, Error> {
        Self::rmw(a, args, Rmw::Or)
    }

    fn xor<'gc>(a: &mut Agent<'gc, '_>, _: Value<'gc>, args: &[Value<'gc>]) -> Result<Value<'gc>, Error> {
        Self::rmw(a, args, Rmw::Xor)
    }

    fn exchange<'gc>(a: &mut Agent<'gc, '_>, _: Value<'gc>, args: &[Value<'gc>]) -> Result<Value<'gc>, Error> {
        Self::rmw(a, args, Rmw::Exchange)
    }

    fn compare_exchange<'gc>(a: &mut Agent<'gc, '_>, _: Value<'gc>, args: &[Value<'gc>]) -> Result<Value<'gc>, Error> {
        let (kind, buffer, at) = Self::atomic_target(a, args.at(0), args.at(1), false)?;
        let expected = Self::atomic_value(a, kind, args.at(2))?;
        let replacement = Self::atomic_value(a, kind, args.at(3))?;
        Self::revalidate(a, buffer, at, kind)?;
        let size = kind.size();
        let old = Self::bytes_of(buffer).unwrap_or_default()[at..at + size].to_vec();
        if old == kind.encode(&expected)
            && let ObjectKind::ArrayBuffer(b) = &mut buffer.borrow_mut(a.mc).kind
            && let Some(dst) = b.bytes.as_mut()
        {
            dst[at..at + size].copy_from_slice(&kind.encode(&replacement));
        }
        Ok(a.element_value(kind.decode(&old)))
    }

    fn load<'gc>(a: &mut Agent<'gc, '_>, _: Value<'gc>, args: &[Value<'gc>]) -> Result<Value<'gc>, Error> {
        let (kind, buffer, at) = Self::atomic_target(a, args.at(0), args.at(1), false)?;
        Self::revalidate(a, buffer, at, kind)?;
        let e = Agent::read_raw(buffer, at, kind).ok_or_else(|| a.throw(ErrorKind::Type, "TypedArray is detached"))?;
        Ok(a.element_value(e))
    }

    fn store<'gc>(a: &mut Agent<'gc, '_>, _: Value<'gc>, args: &[Value<'gc>]) -> Result<Value<'gc>, Error> {
        let (kind, buffer, at) = Self::atomic_target(a, args.at(0), args.at(1), false)?;
        let v = Self::atomic_value(a, kind, args.at(2))?;
        Self::revalidate(a, buffer, at, kind)?;
        a.write_raw(buffer, at, kind, &v);
        Ok(match v {
            r8framework::Element::Number(x) => (x + 0.0).into(),
            r8framework::Element::BigInt(b) => a.bigint(b),
        })
    }

    fn is_lock_free<'gc>(a: &mut Agent<'gc, '_>, _: Value<'gc>, args: &[Value<'gc>]) -> Result<Value<'gc>, Error> {
        let n = a.to_integer_or_infinity(args.at(0))?;
        Ok((n == 1.0 || n == 2.0 || n == 4.0 || n == 8.0).into())
    }

    fn shared(buffer: JsObject<'_>) -> bool {
        matches!(&buffer.borrow().kind, ObjectKind::ArrayBuffer(b) if b.shared)
    }

    /// `Atomics.wait`: nobody can wake us up on a single thread; gives "not-equal" if the value differs, otherwise
    /// "timed-out" per the timeout.
    fn wait<'gc>(a: &mut Agent<'gc, '_>, _: Value<'gc>, args: &[Value<'gc>]) -> Result<Value<'gc>, Error> {
        let (_, view, _) = Self::integer_array(a, args.at(0), true)?;
        if !Self::shared(view.buffer) {
            return Err(a.throw(ErrorKind::Type, "Atomics.wait requires a shared TypedArray"));
        }
        let (kind, buffer, at) = Self::atomic_target(a, args.at(0), args.at(1), true)?;
        let v = if kind.is_bigint() { r8framework::Element::BigInt(Agent::wrap_bigint(&a.to_bigint(args.at(2))?, 64, true)) } else { r8framework::Element::Number(f64::from(a.to_int32(args.at(2))?)) };
        let t = a.to_number(args.at(3))?;
        let _ = if t.is_nan() { f64::INFINITY } else { t.max(0.0) };
        let cur = Self::bytes_of(buffer).unwrap_or_default()[at..at + kind.size()].to_vec();
        let result = if cur == kind.encode(&v) { "timed-out" } else { "not-equal" };
        Ok(a.string(result).into())
    }

    fn notify<'gc>(a: &mut Agent<'gc, '_>, _: Value<'gc>, args: &[Value<'gc>]) -> Result<Value<'gc>, Error> {
        let (_, _, _) = Self::atomic_target(a, args.at(0), args.at(1), true)?;
        if args.at(2) != Value::Undefined {
            a.to_integer_or_infinity(args.at(2))?;
        }
        Ok(0.0.into())
    }

    fn pause<'gc>(a: &mut Agent<'gc, '_>, _: Value<'gc>, args: &[Value<'gc>]) -> Result<Value<'gc>, Error> {
        match args.at(0).kind() {
            ValueKind::Undefined => Ok(Value::Undefined),
            ValueKind::Number(x) if x.is_finite() && libm::trunc(x) == x => Ok(Value::Undefined),
            _ => Err(a.throw(ErrorKind::Type, "Atomics.pause requires an integral number")),
        }
    }
}

impl<'gc> Installer<'_, 'gc> {
    pub(crate) fn buffers(&mut self) {
        let r = self.realm;
        let tag = |o: JsObject<'gc>, t: &str| {
            let v = self.text(t);
            o.borrow_mut(self.mc).define_symbol(r.symbols.to_string_tag, Property { slot: Slot::Data(v), enumerable: false, writable: false, configurable: true });
        };
        let species = |c: JsObject<'gc>| {
            let g = r.named_native(self.mc, Buffers::species, "get [Symbol.species]", 0);
            c.borrow_mut(self.mc).define_symbol(r.symbols.species, Property { slot: Slot::Accessor { get: Some(g), set: None }, enumerable: false, writable: false, configurable: true });
        };
        // ArrayBuffer
        let ab = r.array_buffer_proto;
        let ctor = self.constructor("ArrayBuffer", Buffers::array_buffer, ab, 1);
        self.method(ctor, "isView", Buffers::is_view, 1);
        species(ctor);
        self.getter(ab, "byteLength", Buffers::byte_length::<false>);
        self.getter(ab, "maxByteLength", Buffers::max_byte_length::<false>);
        self.getter(ab, "resizable", Buffers::resizable::<false>);
        self.getter(ab, "detached", Buffers::detached);
        self.method(ab, "slice", Buffers::slice, 2);
        self.method(ab, "resize", Buffers::resize, 1);
        self.method(ab, "transfer", Buffers::transfer, 0);
        self.method(ab, "transferToFixedLength", Buffers::transfer_fixed, 0);
        tag(ab, "ArrayBuffer");
        // SharedArrayBuffer
        let sab = r.shared_array_buffer_proto;
        let ctor = self.constructor("SharedArrayBuffer", Buffers::shared_array_buffer, sab, 1);
        species(ctor);
        self.getter(sab, "byteLength", Buffers::byte_length::<true>);
        self.getter(sab, "maxByteLength", Buffers::max_byte_length::<true>);
        self.getter(sab, "growable", Buffers::resizable::<true>);
        self.method(sab, "slice", Buffers::shared_slice, 2);
        self.method(sab, "grow", Buffers::grow, 1);
        tag(sab, "SharedArrayBuffer");
        // DataView
        let dv = r.data_view_proto;
        self.constructor("DataView", Buffers::data_view, dv, 1);
        self.getter(dv, "buffer", Buffers::view_buffer);
        self.getter(dv, "byteLength", Buffers::view_byte_length);
        self.getter(dv, "byteOffset", Buffers::view_byte_offset);
        let getters: [r8framework::NativeFn; 12] = [
            Buffers::get_value::<0>,
            Buffers::get_value::<1>,
            Buffers::get_value::<2>,
            Buffers::get_value::<3>,
            Buffers::get_value::<4>,
            Buffers::get_value::<5>,
            Buffers::get_value::<6>,
            Buffers::get_value::<7>,
            Buffers::get_value::<8>,
            Buffers::get_value::<9>,
            Buffers::get_value::<10>,
            Buffers::get_value::<11>,
        ];
        let setters: [r8framework::NativeFn; 12] = [
            Buffers::set_value::<0>,
            Buffers::set_value::<1>,
            Buffers::set_value::<2>,
            Buffers::set_value::<3>,
            Buffers::set_value::<4>,
            Buffers::set_value::<5>,
            Buffers::set_value::<6>,
            Buffers::set_value::<7>,
            Buffers::set_value::<8>,
            Buffers::set_value::<9>,
            Buffers::set_value::<10>,
            Buffers::set_value::<11>,
        ];
        for ((kind, g), s) in ElementKind::ALL.into_iter().zip(getters).zip(setters) {
            if kind == ElementKind::Uint8Clamped {
                continue;
            }
            self.method(dv, &alloc::format!("get{}", kind.view_name()), g, 1);
            self.method(dv, &alloc::format!("set{}", kind.view_name()), s, 2);
        }
        tag(dv, "DataView");
        // Atomics
        let atomics = self.plain();
        let methods: [(&str, r8framework::NativeFn, u32); 13] = [
            ("add", Buffers::add, 3),
            ("and", Buffers::and, 3),
            ("compareExchange", Buffers::compare_exchange, 4),
            ("exchange", Buffers::exchange, 3),
            ("isLockFree", Buffers::is_lock_free, 1),
            ("load", Buffers::load, 2),
            ("notify", Buffers::notify, 3),
            ("or", Buffers::or, 3),
            ("pause", Buffers::pause, 0),
            ("store", Buffers::store, 3),
            ("sub", Buffers::sub, 3),
            ("wait", Buffers::wait, 4),
            ("xor", Buffers::xor, 3),
        ];
        for (name, f, n) in methods {
            self.method(atomics, name, f, n);
        }
        tag(atomics, "Atomics");
        self.set(r.global, "Atomics", atomics.into());
    }
}
