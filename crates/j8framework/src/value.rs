use crate::gc::{Collect, Gc, Trace, Tracer, gc_from_raw};

use crate::{JsBigInt, JsObject, JsString, JsSymbol};

#[derive(Clone, Copy, Debug, Default, Collect)]
#[collect(no_drop)]
#[repr(C, u8)]
pub enum ValueKind<'gc> {
    #[default]
    Undefined = ValueTag::UNDEFINED,
    Null = ValueTag::NULL,
    Bool(bool) = ValueTag::BOOL,
    Number(f64) = ValueTag::NUMBER,
    String(JsString<'gc>) = ValueTag::STRING,
    Symbol(JsSymbol<'gc>) = ValueTag::SYMBOL,
    BigInt(JsBigInt<'gc>) = ValueTag::BIGINT,
    Object(JsObject<'gc>) = ValueTag::OBJECT,
    Uninit = ValueTag::UNINIT,
}

#[derive(Clone, Copy)]
#[repr(transparent)]
pub struct Value<'gc> {
    bits: u64,
    _brand: core::marker::PhantomData<JsObject<'gc>>,
}

pub struct ValueBits;

impl ValueBits {
    pub const CANONICAL_NAN: u64 = 0x7FF8_0000_0000_0000;
    pub const TAG_SHIFT: u32 = 48;
    const BOXED: u64 = 0xFFF0_0000_0000_0000;
    pub const BOXED_MIN: u64 = Self::boxed(ValueTag::UNDEFINED, 0);
    pub const PAYLOAD: u64 = (1 << Self::TAG_SHIFT) - 1;
    pub const UNDEFINED: u64 = Self::boxed(ValueTag::UNDEFINED, 0);
    pub const NULL: u64 = Self::boxed(ValueTag::NULL, 0);
    pub const UNINIT: u64 = Self::boxed(ValueTag::UNINIT, 0);
    pub const FALSE: u64 = Self::boxed(ValueTag::BOOL, 0);
    pub const TRUE: u64 = Self::boxed(ValueTag::BOOL, 1);

    const fn boxed(tag: u8, payload: u64) -> u64 {
        Self::BOXED | ((tag as u64 + 1) << Self::TAG_SHIFT) | payload
    }
}

#[allow(non_upper_case_globals, non_snake_case)]
impl<'gc> Value<'gc> {
    pub const Undefined: Self = Self::word(ValueBits::UNDEFINED);
    pub const Null: Self = Self::word(ValueBits::NULL);
    pub const Uninit: Self = Self::word(ValueBits::UNINIT);

    #[inline]
    const fn word(bits: u64) -> Self {
        Self { bits, _brand: core::marker::PhantomData }
    }

    #[inline]
    fn pointer(tag: u8, address: u64) -> Self {
        debug_assert!(address <= ValueBits::PAYLOAD, "GC address beyond 48 bits");
        Self::word(ValueBits::boxed(tag, address))
    }

    #[inline]
    pub const fn Bool(b: bool) -> Self {
        Self::word(if b { ValueBits::TRUE } else { ValueBits::FALSE })
    }

    #[inline]
    pub const fn Number(n: f64) -> Self {
        Self::word(if n.is_nan() { ValueBits::CANONICAL_NAN } else { n.to_bits() })
    }

    #[inline]
    pub fn String(s: JsString<'gc>) -> Self {
        Self::pointer(ValueTag::STRING, s.as_ptr().expose_provenance() as u64)
    }

    #[inline]
    pub fn Symbol(s: JsSymbol<'gc>) -> Self {
        Self::pointer(ValueTag::SYMBOL, s.as_ptr().expose_provenance() as u64)
    }

    #[inline]
    pub fn BigInt(b: JsBigInt<'gc>) -> Self {
        Self::pointer(ValueTag::BIGINT, b.as_ptr().expose_provenance() as u64)
    }

    #[inline]
    pub fn Object(o: JsObject<'gc>) -> Self {
        Self::pointer(ValueTag::OBJECT, o.as_ptr().expose_provenance() as u64)
    }

    #[inline]
    pub fn as_number(self) -> Option<f64> {
        (self.bits < ValueBits::BOXED_MIN).then_some(f64::from_bits(self.bits))
    }

    #[inline]
    pub fn is_number(self) -> bool {
        self.bits < ValueBits::BOXED_MIN
    }

    #[inline]
    pub fn as_object(self) -> Option<JsObject<'gc>> {
        (self.bits >> ValueBits::TAG_SHIFT == ValueBits::boxed(ValueTag::OBJECT, 0) >> ValueBits::TAG_SHIFT)
            .then(|| unsafe { gc_from_raw(Self::address(self.bits & ValueBits::PAYLOAD)) })
    }

    #[inline]
    pub fn as_string(self) -> Option<JsString<'gc>> {
        (self.bits >> ValueBits::TAG_SHIFT == ValueBits::boxed(ValueTag::STRING, 0) >> ValueBits::TAG_SHIFT)
            .then(|| unsafe { gc_from_raw(Self::address(self.bits & ValueBits::PAYLOAD)) })
    }

    #[inline]
    #[allow(clippy::cast_possible_truncation)]
    fn address<T>(payload: u64) -> *const T {
        core::ptr::with_exposed_provenance(payload as usize)
    }

    #[inline]
    pub fn kind(self) -> ValueKind<'gc> {
        let w = self.bits;
        if w < ValueBits::BOXED_MIN {
            return ValueKind::Number(f64::from_bits(w));
        }
        let payload = w & ValueBits::PAYLOAD;
        #[allow(clippy::cast_possible_truncation)]
        let tag = ((w >> ValueBits::TAG_SHIFT) - (ValueBits::BOXED_MIN >> ValueBits::TAG_SHIFT)) as u8;
        match tag {
            ValueTag::UNDEFINED => ValueKind::Undefined,
            ValueTag::NULL => ValueKind::Null,
            ValueTag::UNINIT => ValueKind::Uninit,
            ValueTag::BOOL => ValueKind::Bool(payload != 0),
            ValueTag::STRING => ValueKind::String(unsafe { gc_from_raw(Self::address(payload)) }),
            ValueTag::SYMBOL => ValueKind::Symbol(unsafe { gc_from_raw(Self::address(payload)) }),
            ValueTag::BIGINT => ValueKind::BigInt(unsafe { gc_from_raw(Self::address(payload)) }),
            ValueTag::OBJECT => ValueKind::Object(unsafe { gc_from_raw(Self::address(payload)) }),
            tag => unreachable!("boxed value with tag {tag}"),
        }
    }
}

impl<'gc> From<ValueKind<'gc>> for Value<'gc> {
    #[inline]
    fn from(k: ValueKind<'gc>) -> Self {
        match k {
            ValueKind::Undefined => Value::Undefined,
            ValueKind::Null => Value::Null,
            ValueKind::Uninit => Value::Uninit,
            ValueKind::Bool(b) => Value::Bool(b),
            ValueKind::Number(n) => Value::Number(n),
            ValueKind::String(s) => Value::String(s),
            ValueKind::Symbol(s) => Value::Symbol(s),
            ValueKind::BigInt(b) => Value::BigInt(b),
            ValueKind::Object(o) => Value::Object(o),
        }
    }
}

impl Default for Value<'_> {
    fn default() -> Self {
        Value::Undefined
    }
}

impl core::fmt::Debug for Value<'_> {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        if let Some(l) = self.as_lazy() {
            return core::fmt::Debug::fmt(&l, f);
        }
        if self.as_accessor().is_some() || self.as_context().is_some() {
            return f.write_str("<internal>");
        }
        core::fmt::Debug::fmt(&self.kind(), f)
    }
}

unsafe impl Trace for Value<'_> {
    #[inline]
    fn trace(&self, t: &mut Tracer<'_>) {
        if !self.is_pointer() {
            return;
        }
        if let Some(a) = self.as_accessor() {
            return t.edge(a);
        }
        if let Some(c) = self.as_context() {
            return t.edge(c);
        }
        match self.kind() {
            ValueKind::String(s) => t.edge(s),
            ValueKind::Symbol(s) => t.edge(s),
            ValueKind::BigInt(b) => t.edge(b),
            ValueKind::Object(o) => t.edge(o),
            _ => {}
        }
    }

    #[inline]
    fn relocate(&mut self, t: &mut Tracer<'_>) {
        if !self.is_pointer() {
            return;
        }
        if let Some(mut a) = self.as_accessor() {
            t.relocate(&mut a);
            *self = Value::accessor(a);
            return;
        }
        if let Some(mut c) = self.as_context() {
            t.relocate(&mut c);
            *self = Value::context(c);
            return;
        }
        *self = match self.kind() {
            ValueKind::String(mut s) => {
                t.relocate(&mut s);
                Value::String(s)
            }
            ValueKind::Symbol(mut s) => {
                t.relocate(&mut s);
                Value::Symbol(s)
            }
            ValueKind::BigInt(mut b) => {
                t.relocate(&mut b);
                Value::BigInt(b)
            }
            ValueKind::Object(mut o) => {
                t.relocate(&mut o);
                Value::Object(o)
            }
            _ => return,
        };
    }
}

impl<'gc> Value<'gc> {
    #[inline]
    pub fn bits(self) -> u64 {
        self.bits
    }

    #[inline]
    pub unsafe fn from_bits(bits: u64) -> Self {
        Self::word(bits)
    }

    #[inline]
    pub fn is_pointer(self) -> bool {
        self.bits >= paragc::WORD_POINTER_MIN
    }

    #[inline]
    pub fn lazy(l: crate::Lazy) -> Self {
        Self::word(ValueBits::boxed(ValueTag::LAZY, u64::from(l)))
    }

    pub const LAZY_TAG: u64 = ValueBits::boxed(ValueTag::LAZY, 0) >> ValueBits::TAG_SHIFT;

    #[inline]
    pub fn as_lazy(self) -> Option<crate::Lazy> {
        (self.bits >> ValueBits::TAG_SHIFT == Self::LAZY_TAG)
            .then(|| crate::Lazy::try_from(self.bits & ValueBits::PAYLOAD).ok())
            .flatten()
    }

    #[inline]
    pub fn is_plain_slot(self) -> bool {
        self.as_accessor().is_none() && self.as_lazy().is_none()
    }

    #[inline]
    pub fn accessor(a: Gc<'gc, crate::Accessor<'gc>>) -> Self {
        Self::pointer(ValueTag::ACCESSOR, a.as_ptr().expose_provenance() as u64)
    }

    #[inline]
    pub fn as_accessor(self) -> Option<Gc<'gc, crate::Accessor<'gc>>> {
        (self.bits >> ValueBits::TAG_SHIFT == ValueBits::boxed(ValueTag::ACCESSOR, 0) >> ValueBits::TAG_SHIFT)
            .then(|| unsafe { gc_from_raw(Self::address(self.bits & ValueBits::PAYLOAD)) })
    }

    #[inline]
    pub fn context(e: crate::JsEnv<'gc>) -> Self {
        Self::pointer(ValueTag::CONTEXT, e.as_ptr().expose_provenance() as u64)
    }

    #[inline]
    pub fn as_context(self) -> Option<crate::JsEnv<'gc>> {
        (self.bits >> ValueBits::TAG_SHIFT == ValueBits::boxed(ValueTag::CONTEXT, 0) >> ValueBits::TAG_SHIFT && self.bits & ValueBits::PAYLOAD != 0)
            .then(|| unsafe { gc_from_raw(Self::address(self.bits & ValueBits::PAYLOAD)) })
    }
}

pub struct ValueTag;

impl ValueTag {
    pub const UNDEFINED: u8 = 0;
    pub const NULL: u8 = 1;
    pub const BOOL: u8 = 2;
    pub const NUMBER: u8 = 3;
    pub const UNINIT: u8 = 4;
    pub const LAZY: u8 = 5;
    pub const STRING: u8 = 7;
    pub const SYMBOL: u8 = 8;
    pub const BIGINT: u8 = 9;
    pub const OBJECT: u8 = 10;
    pub const ACCESSOR: u8 = 11;
    pub const CONTEXT: u8 = 12;
    pub const SIZE: usize = 8;
}

const _: () = assert!(core::mem::size_of::<Value<'static>>() == ValueTag::SIZE);

const _: () = {
    let pointers = [ValueTag::STRING, ValueTag::SYMBOL, ValueTag::BIGINT, ValueTag::OBJECT, ValueTag::ACCESSOR, ValueTag::CONTEXT];
    let mut i = 0;
    while i < pointers.len() {
        let w = ValueBits::boxed(pointers[i], 1);
        assert!(w >= paragc::WORD_POINTER_MIN && w < paragc::RAW_TAG);
        i += 1;
    }
    let others = [ValueTag::UNDEFINED, ValueTag::NULL, ValueTag::BOOL, ValueTag::UNINIT, ValueTag::LAZY];
    let mut i = 0;
    while i < others.len() {
        assert!(ValueBits::boxed(others[i], 1) < paragc::WORD_POINTER_MIN);
        i += 1;
    }
};

impl PartialEq for Value<'_> {
    #[inline]
    fn eq(&self, o: &Self) -> bool {
        match (self.kind(), o.kind()) {
            (ValueKind::Undefined, ValueKind::Undefined) | (ValueKind::Null, ValueKind::Null) | (ValueKind::Uninit, ValueKind::Uninit) => true,
            (ValueKind::Bool(a), ValueKind::Bool(b)) => a == b,
            (ValueKind::Number(a), ValueKind::Number(b)) => a == b,
            (ValueKind::String(a), ValueKind::String(b)) => *a == *b,
            (ValueKind::Object(a), ValueKind::Object(b)) => Gc::ptr_eq(a, b),
            (ValueKind::Symbol(a), ValueKind::Symbol(b)) => Gc::ptr_eq(a, b),
            (ValueKind::BigInt(a), ValueKind::BigInt(b)) => a.0 == b.0,
            _ => false,
        }
    }
}

impl<'gc> Value<'gc> {
    pub fn from_proto(p: Option<crate::JsObject<'gc>>) -> Self {
        p.map_or(Value::Null, Value::Object)
    }
}

impl Value<'_> {
    pub fn same_value(self, o: Self) -> bool {
        match (self.kind(), o.kind()) {
            (ValueKind::Number(a), ValueKind::Number(b)) => (a.is_nan() && b.is_nan()) || (a == b && a.is_sign_negative() == b.is_sign_negative()),
            _ => self == o,
        }
    }

    pub fn is_nullish(self) -> bool {
        matches!(self.kind(), ValueKind::Undefined | ValueKind::Null)
    }

    #[must_use]
    pub fn unwrapped(self) -> Self {
        match self.kind() {
            ValueKind::Object(o) => match o.borrow().kind {
                crate::ObjectKind::Primitive(v) => v,
                _ => self,
            },
            _ => self,
        }
    }
}

impl From<bool> for Value<'_> {
    fn from(b: bool) -> Self {
        Value::Bool(b)
    }
}

impl From<f64> for Value<'_> {
    fn from(x: f64) -> Self {
        Value::Number(x)
    }
}

impl<'gc> From<JsString<'gc>> for Value<'gc> {
    fn from(s: JsString<'gc>) -> Self {
        Value::String(s)
    }
}

impl<'gc> From<JsSymbol<'gc>> for Value<'gc> {
    fn from(s: JsSymbol<'gc>) -> Self {
        Value::Symbol(s)
    }
}

impl<'gc> From<JsObject<'gc>> for Value<'gc> {
    fn from(o: JsObject<'gc>) -> Self {
        Value::Object(o)
    }
}

#[cfg(test)]
mod tests {
    use super::Value;

    #[test]
    fn word_round_trip() {
        for v in [Value::Undefined, Value::Null, Value::Uninit, Value::Bool(true), Value::Bool(false)] {
            assert!(unsafe { Value::from_bits(v.bits()) }.same_value(v));
        }
        for n in [0.0, -0.0, 1.5, -2.0, f64::INFINITY, f64::NEG_INFINITY, f64::MAX, f64::MIN_POSITIVE, f64::NAN, -f64::NAN] {
            assert!(unsafe { Value::from_bits(Value::Number(n).bits()) }.same_value(Value::Number(n)), "{n}");
        }
    }
}
