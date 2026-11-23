use j8framework::ValueKind;
use alloc::boxed::Box;

use j8framework::{Agent, Backing, BufferData, ElementKind, Error, ErrorKind, JsObject, ObjectKind, Property, Slot, Value, ViewData};

use crate::{Args, Installer};

pub(crate) struct Buffers;

const MAX_BYTES: usize = 1 << 31;

#[derive(Clone, Copy, PartialEq, Eq)]
enum Rmw {
    Add,
    Sub,
    And,
    Or,
    Xor,
    Exchange,
}

impl Rmw {
    fn apply(self, x: u64, y: u64) -> u64 {
        match self {
            Self::Add => x.wrapping_add(y),
            Self::Sub => x.wrapping_sub(y),
            Self::And => x & y,
            Self::Or => x | y,
            Self::Xor => x ^ y,
            Self::Exchange => y,
        }
    }
}

impl Buffers {
    pub(crate) fn allocate<'scope>(a: &mut Agent<'scope, '_>, proto: JsObject<'scope>, len: usize, max: Option<usize>, shared: bool) -> Result<JsObject<'scope>, Error> {
        if len > MAX_BYTES || max.is_some_and(|m| m > MAX_BYTES) {
            return Err(a.throw(ErrorKind::Range, "Array buffer allocation failed"));
        }
        let bytes = if shared {
            j8framework::SharedMemory::new(len, max).ok_or_else(|| a.throw(ErrorKind::Range, "Array buffer allocation failed"))?.into()
        } else {
            Backing::from(alloc::vec![0; len])
        };
        let data = BufferData { bytes: Some(bytes), max, shared };
        Ok(j8framework::ObjectData { kind: ObjectKind::ArrayBuffer(Box::new(data)), proto: Some(proto), ..j8framework::ObjectData::default() }.alloc(a.mc))
    }

    fn this_buffer<'scope>(a: &Agent<'scope, '_>, this: Value<'scope>, shared: bool) -> Result<JsObject<'scope>, Error> {
        match this.kind() {
            ValueKind::Object(o) if matches!(&o.borrow().kind, ObjectKind::ArrayBuffer(b) if b.shared == shared) => Ok(o),
            _ => Err(a.throw(ErrorKind::Type, if shared { "Method called on a non-SharedArrayBuffer" } else { "Method called on a non-ArrayBuffer" })),
        }
    }

    fn info(o: JsObject<'_>) -> (Option<usize>, Option<usize>) {
        match &o.borrow().kind {
            ObjectKind::ArrayBuffer(b) => (b.bytes.as_deref().map(<[u8]>::len), b.max),
            _ => (None, None),
        }
    }

    fn construct_with<'scope>(a: &mut Agent<'scope, '_>, args: &[Value<'scope>], new_target: Value<'scope>, shared: bool) -> Result<Value<'scope>, Error> {
        if new_target == Value::Undefined {
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
        let fallback = if shared { a.context.shared_array_buffer_proto } else { a.context.array_buffer_proto };
        let proto = a.proto_from_ctor(new_target, fallback)?;
        Ok(Self::allocate(a, proto, len, max, shared)?.into())
    }

    fn array_buffer<'scope>(a: &mut Agent<'scope, '_>, _: Value<'scope>, args: &[Value<'scope>]) -> Result<Value<'scope>, Error> {
        Self::construct_with(a, args, a.new_target, false)
    }

    fn shared_array_buffer<'scope>(a: &mut Agent<'scope, '_>, _: Value<'scope>, args: &[Value<'scope>]) -> Result<Value<'scope>, Error> {
        Self::construct_with(a, args, a.new_target, true)
    }

    fn is_view<'scope>(_: &mut Agent<'scope, '_>, _: Value<'scope>, args: &[Value<'scope>]) -> Result<Value<'scope>, Error> {
        Ok(matches!(args.at(0).kind(), ValueKind::Object(o) if matches!(o.borrow().kind, ObjectKind::TypedArray(..) | ObjectKind::DataView(_))).into())
    }

    fn species<'scope>(_: &mut Agent<'scope, '_>, this: Value<'scope>, _: &[Value<'scope>]) -> Result<Value<'scope>, Error> {
        Ok(this)
    }

    fn byte_length<'scope, const SHARED: bool>(a: &mut Agent<'scope, '_>, this: Value<'scope>, _: &[Value<'scope>]) -> Result<Value<'scope>, Error> {
        let o = Self::this_buffer(a, this, SHARED)?;
        Ok(Self::number(Self::info(o).0.unwrap_or(0)))
    }

    fn number<'scope>(n: usize) -> Value<'scope> {
        #[expect(clippy::cast_precision_loss, reason = "byte count is at most 2^31")]
        let x = n as f64;
        x.into()
    }

    fn max_byte_length<'scope, const SHARED: bool>(a: &mut Agent<'scope, '_>, this: Value<'scope>, _: &[Value<'scope>]) -> Result<Value<'scope>, Error> {
        let o = Self::this_buffer(a, this, SHARED)?;
        let (len, max) = Self::info(o);
        Ok(Self::number(match len {
            None => 0,
            Some(l) => max.unwrap_or(l),
        }))
    }

    fn resizable<'scope, const SHARED: bool>(a: &mut Agent<'scope, '_>, this: Value<'scope>, _: &[Value<'scope>]) -> Result<Value<'scope>, Error> {
        let o = Self::this_buffer(a, this, SHARED)?;
        Ok(Self::info(o).1.is_some().into())
    }

    fn detached<'scope>(a: &mut Agent<'scope, '_>, this: Value<'scope>, _: &[Value<'scope>]) -> Result<Value<'scope>, Error> {
        let o = Self::this_buffer(a, this, false)?;
        Ok(Self::info(o).0.is_none().into())
    }

    fn set_bytes<'scope>(a: &Agent<'scope, '_>, o: JsObject<'scope>, bytes: Option<Backing>) {
        if let ObjectKind::ArrayBuffer(b) = &mut o.borrow_mut(a.mc).kind {
            b.bytes = bytes;
        }
    }

    fn take_bytes<'scope>(a: &Agent<'scope, '_>, o: JsObject<'scope>) -> Option<Backing> {
        match &mut o.borrow_mut(a.mc).kind {
            ObjectKind::ArrayBuffer(b) => b.bytes.take(),
            _ => None,
        }
    }

    fn read(o: JsObject<'_>, at: usize, size: usize) -> [u8; 8] {
        let mut x = [0u8; 8];
        if let ObjectKind::ArrayBuffer(b) = &o.borrow().kind
            && let Some(src) = b.bytes.as_deref().and_then(|bytes| bytes.get(at..at + size))
        {
            x[..size].copy_from_slice(src);
        }
        x
    }

    fn update<'scope>(a: &Agent<'scope, '_>, o: JsObject<'scope>, at: usize, size: usize, f: impl Fn(u64) -> Option<u64>) -> u64 {
        match &mut o.borrow_mut(a.mc).kind {
            ObjectKind::ArrayBuffer(b) => b.bytes.as_mut().map_or(0, |bytes| bytes.update(at, size, f)),
            _ => 0,
        }
    }

    fn resize_with<'scope>(a: &mut Agent<'scope, '_>, this: Value<'scope>, args: &[Value<'scope>], shared: bool) -> Result<Value<'scope>, Error> {
        let o = Self::this_buffer(a, this, shared)?;
        let Some(max) = Self::info(o).1 else {
            return Err(a.throw(ErrorKind::Type, "Method called on a fixed-length buffer"));
        };
        let n = a.to_index(args.at(0))?;
        let resized = match &mut o.borrow_mut(a.mc).kind {
            ObjectKind::ArrayBuffer(b) => b.bytes.as_mut().map(|bytes| n <= max && !(shared && n < bytes.len()) && bytes.resize(n)),
            _ => None,
        };
        match resized {
            None => Err(a.throw(ErrorKind::Type, "Cannot resize a detached ArrayBuffer")),
            Some(false) => Err(a.throw(ErrorKind::Range, "Invalid length for resize")),
            Some(true) => Ok(Value::Undefined),
        }
    }

    fn resize<'scope>(a: &mut Agent<'scope, '_>, this: Value<'scope>, args: &[Value<'scope>]) -> Result<Value<'scope>, Error> {
        Self::resize_with(a, this, args, false)
    }

    fn grow<'scope>(a: &mut Agent<'scope, '_>, this: Value<'scope>, args: &[Value<'scope>]) -> Result<Value<'scope>, Error> {
        Self::resize_with(a, this, args, true)
    }

    pub(crate) fn species_constructor<'scope>(a: &mut Agent<'scope, '_>, o: Value<'scope>, fallback: Value<'scope>) -> Result<Value<'scope>, Error> {
        let c = a.get_named(o, "constructor")?;
        if c == Value::Undefined {
            return Ok(fallback);
        }
        if c.as_object().is_none() {
            return Err(a.throw(ErrorKind::Type, "object.constructor is not an object"));
        }
        let s = a.get_symbol(c, a.context.symbols.species, c)?;
        if s.is_nullish() {
            return Ok(fallback);
        }
        if a.is_constructor(s) {
            return Ok(s);
        }
        Err(a.throw(ErrorKind::Type, "object.constructor[Symbol.species] is not a constructor"))
    }

    fn global_ctor<'scope>(a: &Agent<'scope, '_>, name: &str) -> Value<'scope> {
        a.context.global.borrow().own(a.mc, name).unwrap_or_default()
    }

    fn slice_with<'scope>(a: &mut Agent<'scope, '_>, this: Value<'scope>, args: &[Value<'scope>], shared: bool) -> Result<Value<'scope>, Error> {
        let o = Self::this_buffer(a, this, shared)?;
        let Some(len) = Self::info(o).0 else {
            return Err(a.throw(ErrorKind::Type, "Cannot slice a detached ArrayBuffer"));
        };
        #[expect(clippy::cast_precision_loss, reason = "byte count is at most 2^31")]
        let lenf = len as f64;
        let rel = |a: &mut Agent<'scope, '_>, v: Value<'scope>, default: f64| -> Result<f64, Error> {
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
        let Some(n) = made.as_object() else { return Err(a.throw(ErrorKind::Type, "species constructor returned a non-object")) };
        let ok = matches!(&n.borrow().kind, ObjectKind::ArrayBuffer(b) if b.shared == shared && b.bytes.is_some());
        if !ok {
            return Err(a.throw(ErrorKind::Type, "species constructor did not return a usable buffer"));
        }
        if j8framework::ScopeLocal::ptr_eq(n, o) {
            return Err(a.throw(ErrorKind::Type, "species constructor returned the same buffer"));
        }
        #[expect(clippy::cast_possible_truncation, clippy::cast_sign_loss, reason = "clamped to [0, len]")]
        let (first, new_len) = (first as usize, new_len as usize);
        if Self::info(n).0.unwrap_or(0) < new_len {
            return Err(a.throw(ErrorKind::Type, "species constructor returned a too small buffer"));
        }
        let Some(len) = Self::info(o).0 else {
            return Err(a.throw(ErrorKind::Type, "ArrayBuffer was detached"));
        };
        let end = (first + new_len).min(len);
        if first < end
            && let ObjectKind::ArrayBuffer(sb) = &o.borrow().kind
            && let Some(src) = sb.bytes.as_deref()
            && let ObjectKind::ArrayBuffer(b) = &mut n.borrow_mut(a.mc).kind
            && let Some(dst) = b.bytes.as_mut()
        {
            dst[..end - first].copy_from_slice(&src[first..end]);
        }
        Ok(n.into())
    }

    fn slice<'scope>(a: &mut Agent<'scope, '_>, this: Value<'scope>, args: &[Value<'scope>]) -> Result<Value<'scope>, Error> {
        Self::slice_with(a, this, args, false)
    }

    fn shared_slice<'scope>(a: &mut Agent<'scope, '_>, this: Value<'scope>, args: &[Value<'scope>]) -> Result<Value<'scope>, Error> {
        Self::slice_with(a, this, args, true)
    }

    fn transfer_with<'scope>(a: &mut Agent<'scope, '_>, this: Value<'scope>, args: &[Value<'scope>], keep_resizable: bool) -> Result<Value<'scope>, Error> {
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
        let n = Self::allocate(a, a.context.array_buffer_proto, 0, max, false)?;
        let mut bytes = Self::take_bytes(a, o).unwrap_or_else(|| Backing::from(alloc::vec::Vec::new()));
        if !bytes.resize(new_len) {
            let mut v = bytes.into_vec();
            v.resize(new_len, 0);
            bytes = v.into();
        }
        Self::set_bytes(a, n, Some(bytes));
        Ok(n.into())
    }

    fn transfer<'scope>(a: &mut Agent<'scope, '_>, this: Value<'scope>, args: &[Value<'scope>]) -> Result<Value<'scope>, Error> {
        Self::transfer_with(a, this, args, true)
    }

    fn transfer_fixed<'scope>(a: &mut Agent<'scope, '_>, this: Value<'scope>, args: &[Value<'scope>]) -> Result<Value<'scope>, Error> {
        Self::transfer_with(a, this, args, false)
    }

    fn data_view<'scope>(a: &mut Agent<'scope, '_>, _: Value<'scope>, args: &[Value<'scope>]) -> Result<Value<'scope>, Error> {
        let new_target = a.new_target;
        if new_target == Value::Undefined {
            return Err(a.throw(ErrorKind::Type, "Constructor DataView requires 'new'"));
        }
        let Some(buffer) = args.at(0).as_object() else { return Err(a.throw(ErrorKind::Type, "First argument to DataView constructor must be an ArrayBuffer")) };
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
        let proto = a.proto_from_ctor(new_target, a.context.data_view_proto)?;
        let Some(len) = Self::info(buffer).0 else { return Err(a.throw(ErrorKind::Type, "Cannot construct a DataView on a detached ArrayBuffer")) };
        if offset > len || length.is_some_and(|n| offset + n > len) {
            return Err(a.throw(ErrorKind::Range, "Invalid DataView length"));
        }
        let view = ViewData { buffer, offset, length };
        Ok(j8framework::ObjectData { kind: ObjectKind::DataView(view), proto: Some(proto), ..j8framework::ObjectData::default() }.alloc(a.mc).into())
    }

    fn this_view<'scope>(a: &Agent<'scope, '_>, this: Value<'scope>) -> Result<ViewData<'scope>, Error> {
        match this.kind() {
            ValueKind::Object(o) => match o.borrow().kind {
                ObjectKind::DataView(v) => Ok(v),
                _ => Err(a.throw(ErrorKind::Type, "Method called on a non-DataView")),
            },
            _ => Err(a.throw(ErrorKind::Type, "Method called on a non-DataView")),
        }
    }

    fn view_buffer<'scope>(a: &mut Agent<'scope, '_>, this: Value<'scope>, _: &[Value<'scope>]) -> Result<Value<'scope>, Error> {
        Ok(Self::this_view(a, this)?.buffer.into())
    }

    fn view_byte_length<'scope>(a: &mut Agent<'scope, '_>, this: Value<'scope>, _: &[Value<'scope>]) -> Result<Value<'scope>, Error> {
        let v = Self::this_view(a, this)?;
        match Agent::view_length(&v, 1) {
            Some(n) => Ok(Self::number(n)),
            None => Err(a.throw(ErrorKind::Type, "DataView is out of bounds")),
        }
    }

    fn view_byte_offset<'scope>(a: &mut Agent<'scope, '_>, this: Value<'scope>, _: &[Value<'scope>]) -> Result<Value<'scope>, Error> {
        let v = Self::this_view(a, this)?;
        match Agent::view_length(&v, 1) {
            Some(_) => Ok(Self::number(v.offset)),
            None => Err(a.throw(ErrorKind::Type, "DataView is out of bounds")),
        }
    }

    fn get_value<'scope, const K: usize>(a: &mut Agent<'scope, '_>, this: Value<'scope>, args: &[Value<'scope>]) -> Result<Value<'scope>, Error> {
        let kind = ElementKind::ALL[K];
        let v = Self::this_view(a, this)?;
        let index = a.to_index(args.at(0))?;
        let little = a.to_boolean(args.at(1));
        let at = Self::view_access(a, &v, index, kind)?;
        let mut x = Self::read(v.buffer, at, kind.size());
        let bytes = &mut x[..kind.size()];
        if !little {
            bytes.reverse();
        }
        Ok(a.element_value(kind.decode(bytes)))
    }

    fn view_access<'scope>(a: &Agent<'scope, '_>, v: &ViewData<'scope>, index: usize, kind: ElementKind) -> Result<usize, Error> {
        let Some(len) = Agent::view_length(v, 1) else { return Err(a.throw(ErrorKind::Type, "DataView is out of bounds")) };
        if index + kind.size() > len {
            return Err(a.throw(ErrorKind::Range, "Offset is outside the bounds of the DataView"));
        }
        Ok(v.offset + index)
    }

    fn set_value<'scope, const K: usize>(a: &mut Agent<'scope, '_>, this: Value<'scope>, args: &[Value<'scope>]) -> Result<Value<'scope>, Error> {
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

    fn atomic_target<'scope>(a: &mut Agent<'scope, '_>, ta: Value<'scope>, index: Value<'scope>, waitable: bool) -> Result<(ElementKind, JsObject<'scope>, usize), Error> {
        let (kind, view, len) = Self::integer_array(a, ta, waitable)?;
        let i = a.to_index(index)?;
        if i >= len {
            return Err(a.throw(ErrorKind::Range, "Atomics access index out of range"));
        }
        Ok((kind, view.buffer, view.offset + i * kind.size()))
    }

    fn integer_array<'scope>(a: &mut Agent<'scope, '_>, ta: Value<'scope>, waitable: bool) -> Result<(ElementKind, ViewData<'scope>, usize), Error> {
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

    fn revalidate<'scope>(a: &Agent<'scope, '_>, buffer: JsObject<'scope>, at: usize, kind: ElementKind) -> Result<(), Error> {
        match Self::info(buffer).0 {
            None => Err(a.throw(ErrorKind::Type, "TypedArray is detached")),
            Some(len) if at + kind.size() > len => Err(a.throw(ErrorKind::Range, "Atomics access index out of range")),
            Some(_) => Ok(()),
        }
    }

    fn atomic_value<'scope>(a: &mut Agent<'scope, '_>, kind: ElementKind, v: Value<'scope>) -> Result<j8framework::Element, Error> {
        if kind.is_bigint() { Ok(j8framework::Element::BigInt(a.to_bigint(v)?)) } else { Ok(j8framework::Element::Number(a.to_integer_or_infinity(v)?)) }
    }

    fn raw(bytes: &[u8]) -> u64 {
        let mut x = [0u8; 8];
        x[..bytes.len()].copy_from_slice(bytes);
        u64::from_le_bytes(x)
    }

    fn rmw<'scope>(a: &mut Agent<'scope, '_>, args: &[Value<'scope>], op: Rmw) -> Result<Value<'scope>, Error> {
        let (kind, buffer, at) = Self::atomic_target(a, args.at(0), args.at(1), false)?;
        let v = Self::atomic_value(a, kind, args.at(2))?;
        Self::revalidate(a, buffer, at, kind)?;
        let y = Self::raw(&kind.encode(&v));
        let old = Self::update(a, buffer, at, kind.size(), |x| Some(op.apply(x, y)));
        Ok(a.element_value(kind.decode(&old.to_le_bytes()[..kind.size()])))
    }

    fn add<'scope>(a: &mut Agent<'scope, '_>, _: Value<'scope>, args: &[Value<'scope>]) -> Result<Value<'scope>, Error> {
        Self::rmw(a, args, Rmw::Add)
    }

    fn sub<'scope>(a: &mut Agent<'scope, '_>, _: Value<'scope>, args: &[Value<'scope>]) -> Result<Value<'scope>, Error> {
        Self::rmw(a, args, Rmw::Sub)
    }

    fn and<'scope>(a: &mut Agent<'scope, '_>, _: Value<'scope>, args: &[Value<'scope>]) -> Result<Value<'scope>, Error> {
        Self::rmw(a, args, Rmw::And)
    }

    fn or<'scope>(a: &mut Agent<'scope, '_>, _: Value<'scope>, args: &[Value<'scope>]) -> Result<Value<'scope>, Error> {
        Self::rmw(a, args, Rmw::Or)
    }

    fn xor<'scope>(a: &mut Agent<'scope, '_>, _: Value<'scope>, args: &[Value<'scope>]) -> Result<Value<'scope>, Error> {
        Self::rmw(a, args, Rmw::Xor)
    }

    fn exchange<'scope>(a: &mut Agent<'scope, '_>, _: Value<'scope>, args: &[Value<'scope>]) -> Result<Value<'scope>, Error> {
        Self::rmw(a, args, Rmw::Exchange)
    }

    fn compare_exchange<'scope>(a: &mut Agent<'scope, '_>, _: Value<'scope>, args: &[Value<'scope>]) -> Result<Value<'scope>, Error> {
        let (kind, buffer, at) = Self::atomic_target(a, args.at(0), args.at(1), false)?;
        let expected = Self::atomic_value(a, kind, args.at(2))?;
        let replacement = Self::atomic_value(a, kind, args.at(3))?;
        Self::revalidate(a, buffer, at, kind)?;
        let (e, r) = (Self::raw(&kind.encode(&expected)), Self::raw(&kind.encode(&replacement)));
        let old = Self::update(a, buffer, at, kind.size(), |x| (x == e).then_some(r));
        Ok(a.element_value(kind.decode(&old.to_le_bytes()[..kind.size()])))
    }

    fn load<'scope>(a: &mut Agent<'scope, '_>, _: Value<'scope>, args: &[Value<'scope>]) -> Result<Value<'scope>, Error> {
        let (kind, buffer, at) = Self::atomic_target(a, args.at(0), args.at(1), false)?;
        Self::revalidate(a, buffer, at, kind)?;
        let x = Self::update(a, buffer, at, kind.size(), |_| None);
        Ok(a.element_value(kind.decode(&x.to_le_bytes()[..kind.size()])))
    }

    fn store<'scope>(a: &mut Agent<'scope, '_>, _: Value<'scope>, args: &[Value<'scope>]) -> Result<Value<'scope>, Error> {
        let (kind, buffer, at) = Self::atomic_target(a, args.at(0), args.at(1), false)?;
        let v = Self::atomic_value(a, kind, args.at(2))?;
        Self::revalidate(a, buffer, at, kind)?;
        let y = Self::raw(&kind.encode(&v));
        Self::update(a, buffer, at, kind.size(), |_| Some(y));
        Ok(match v {
            j8framework::Element::Number(x) => (x + 0.0).into(),
            j8framework::Element::BigInt(b) => a.bigint(b),
        })
    }

    fn is_lock_free<'scope>(a: &mut Agent<'scope, '_>, _: Value<'scope>, args: &[Value<'scope>]) -> Result<Value<'scope>, Error> {
        let n = a.to_integer_or_infinity(args.at(0))?;
        Ok((n == 1.0 || n == 2.0 || n == 4.0 || n == 8.0).into())
    }

    fn shared(buffer: JsObject<'_>) -> bool {
        matches!(&buffer.borrow().kind, ObjectKind::ArrayBuffer(b) if b.shared)
    }

    fn wait<'scope>(a: &mut Agent<'scope, '_>, _: Value<'scope>, args: &[Value<'scope>]) -> Result<Value<'scope>, Error> {
        let (_, view, _) = Self::integer_array(a, args.at(0), true)?;
        if !Self::shared(view.buffer) {
            return Err(a.throw(ErrorKind::Type, "Atomics.wait requires a shared TypedArray"));
        }
        let (kind, buffer, at) = Self::atomic_target(a, args.at(0), args.at(1), true)?;
        let v = if kind.is_bigint() { j8framework::Element::BigInt(Agent::wrap_bigint(&a.to_bigint(args.at(2))?, 64, true)) } else { j8framework::Element::Number(f64::from(a.to_int32(args.at(2))?)) };
        let t = a.to_number(args.at(3))?;
        let _ = if t.is_nan() { f64::INFINITY } else { t.max(0.0) };
        let cur = Self::update(a, buffer, at, kind.size(), |_| None);
        let result = if cur == Self::raw(&kind.encode(&v)) { "timed-out" } else { "not-equal" };
        Ok(a.string(result).into())
    }

    fn notify<'scope>(a: &mut Agent<'scope, '_>, _: Value<'scope>, args: &[Value<'scope>]) -> Result<Value<'scope>, Error> {
        let (_, _, _) = Self::atomic_target(a, args.at(0), args.at(1), true)?;
        if args.at(2) != Value::Undefined {
            a.to_integer_or_infinity(args.at(2))?;
        }
        Ok(0.0.into())
    }

    fn pause<'scope>(a: &mut Agent<'scope, '_>, _: Value<'scope>, args: &[Value<'scope>]) -> Result<Value<'scope>, Error> {
        match args.at(0).kind() {
            ValueKind::Undefined => Ok(Value::Undefined),
            ValueKind::Number(x) if x.is_finite() && libm::trunc(x) == x => Ok(Value::Undefined),
            _ => Err(a.throw(ErrorKind::Type, "Atomics.pause requires an integral number")),
        }
    }
}

impl<'scope> Installer<'_, 'scope> {
    pub(crate) fn buffers(&mut self) {
        let r = self.context;
        let tag = |o: JsObject<'scope>, t: &str| {
            let v = self.text(t);
            o.borrow_mut(self.mc).define_symbol(r.symbols.to_string_tag, Property { slot: Slot::Data(v), enumerable: false, writable: false, configurable: true });
        };
        let species = |c: JsObject<'scope>| {
            let g = r.named_native(self.mc, Buffers::species, "get [Symbol.species]", 0);
            c.borrow_mut(self.mc).define_symbol(r.symbols.species, Property { slot: Slot::Accessor { get: Some(g), set: None }, enumerable: false, writable: false, configurable: true });
        };
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
        let sab = r.shared_array_buffer_proto;
        let ctor = self.constructor("SharedArrayBuffer", Buffers::shared_array_buffer, sab, 1);
        species(ctor);
        self.getter(sab, "byteLength", Buffers::byte_length::<true>);
        self.getter(sab, "maxByteLength", Buffers::max_byte_length::<true>);
        self.getter(sab, "growable", Buffers::resizable::<true>);
        self.method(sab, "slice", Buffers::shared_slice, 2);
        self.method(sab, "grow", Buffers::grow, 1);
        tag(sab, "SharedArrayBuffer");
        let dv = r.data_view_proto;
        self.constructor("DataView", Buffers::data_view, dv, 1);
        self.getter(dv, "buffer", Buffers::view_buffer);
        self.getter(dv, "byteLength", Buffers::view_byte_length);
        self.getter(dv, "byteOffset", Buffers::view_byte_offset);
        let getters: [j8framework::NativeFn; 12] = [
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
        let setters: [j8framework::NativeFn; 12] = [
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
        let atomics = self.plain();
        let methods: [(&str, j8framework::NativeFn, u32); 13] = [
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
