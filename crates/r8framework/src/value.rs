use r8gc::{Collect, Gc};

use crate::{JsBigInt, JsObject, JsString, JsSymbol};

/// What a [`Value`] is, for matching: `match v.kind() { ValueKind::Number(n) => … }`. Scalars are stored directly,
/// strings and objects are GC pointers living in `'gc`.
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
    /// A `let` / `const` / `class` binding in the temporal dead zone that is not yet initialized (and `this` before
    /// `super()` in a derived class). Appears only in registers, environment slots, and the script scope; checked away
    /// before reads and writes, so it never escapes to JS.
    Uninit = ValueTag::UNINIT,
}

/// A JS value: one machine word, NaN-boxed (see [`ValueBits`]). Build one with `Value::Number(x)`,
/// `Value::Undefined`, `Value::Object(o)` … (or `From`); look inside with [`Value::kind`].
///
/// The registers, the accumulator, environment and property slots all hold this same word, so JIT-generated machine
/// code reads and writes it directly and an atomic slot stores it as is (`r8gc::Lock<Value>`).
#[derive(Clone, Copy)]
#[repr(transparent)]
pub struct Value<'gc> {
    bits: u64,
    /// Behaves as if it held a `JsObject<'gc>` (brand, variance, auto traits)
    _brand: core::marker::PhantomData<JsObject<'gc>>,
}

/// The word layout of a [`Value`]. Numbers are their own `f64` bits (every NaN is stored as [`Self::CANONICAL_NAN`]).
/// Everything else lives in the negative NaN space: the top 16 bits are `0xFFF0 | (tag + 1)` with the tag from
/// [`ValueTag`] (so never `0xFFF0`, which is `-Infinity`), and the low 48 bits carry the payload: a bool, or a GC
/// address (user-space addresses fit in 48 bits).
pub struct ValueBits;

impl ValueBits {
    /// Every number NaN is stored as this
    pub const CANONICAL_NAN: u64 = 0x7FF8_0000_0000_0000;
    /// Where the (tag + 1) nibble sits
    pub const TAG_SHIFT: u32 = 48;
    /// The top 12 bits every boxed value starts with
    const BOXED: u64 = 0xFFF0_0000_0000_0000;
    /// The smallest boxed word (tag 0, payload 0): every word below it (unsigned) is a number
    pub const BOXED_MIN: u64 = Self::boxed(ValueTag::UNDEFINED, 0);
    /// The low 48 bits: the payload
    pub const PAYLOAD: u64 = (1 << Self::TAG_SHIFT) - 1;
    pub const UNDEFINED: u64 = Self::boxed(ValueTag::UNDEFINED, 0);
    pub const NULL: u64 = Self::boxed(ValueTag::NULL, 0);
    pub const UNINIT: u64 = Self::boxed(ValueTag::UNINIT, 0);
    pub const FALSE: u64 = Self::boxed(ValueTag::BOOL, 0);
    pub const TRUE: u64 = Self::boxed(ValueTag::BOOL, 1);

    /// A boxed word
    const fn boxed(tag: u8, payload: u64) -> u64 {
        Self::BOXED | ((tag as u64 + 1) << Self::TAG_SHIFT) | payload
    }
}

/// Constructors named like the kinds, so building a value reads the same everywhere
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
        Self::pointer(ValueTag::STRING, r8gc::Word::into_word(s))
    }

    #[inline]
    pub fn Symbol(s: JsSymbol<'gc>) -> Self {
        Self::pointer(ValueTag::SYMBOL, r8gc::Word::into_word(s))
    }

    #[inline]
    pub fn BigInt(b: JsBigInt<'gc>) -> Self {
        Self::pointer(ValueTag::BIGINT, r8gc::Word::into_word(b))
    }

    #[inline]
    pub fn Object(o: JsObject<'gc>) -> Self {
        Self::pointer(ValueTag::OBJECT, r8gc::Word::into_word(o))
    }

    /// The number, if it is one (one compare: no full decode)
    #[inline]
    pub fn as_number(self) -> Option<f64> {
        (self.bits < ValueBits::BOXED_MIN).then_some(f64::from_bits(self.bits))
    }

    /// Whether it is a number (one compare)
    #[inline]
    pub fn is_number(self) -> bool {
        self.bits < ValueBits::BOXED_MIN
    }

    /// The object, if it is one (one compare of the top 16 bits)
    #[inline]
    pub fn as_object(self) -> Option<JsObject<'gc>> {
        (self.bits >> ValueBits::TAG_SHIFT == ValueBits::boxed(ValueTag::OBJECT, 0) >> ValueBits::TAG_SHIFT)
            // SAFETY: an object word was built from a live object's address
            .then(|| unsafe { Gc::from_addr_unchecked(Self::address(self.bits & ValueBits::PAYLOAD)) })
    }

    /// A payload as an address (48 bits always fit a `usize` on the 64-bit targets the word layout is for)
    #[inline]
    #[allow(clippy::cast_possible_truncation)]
    const fn address(payload: u64) -> usize {
        payload as usize
    }

    /// What the value is
    #[inline]
    pub fn kind(self) -> ValueKind<'gc> {
        let w = self.bits;
        if w < ValueBits::BOXED_MIN {
            return ValueKind::Number(f64::from_bits(w));
        }
        let payload = w & ValueBits::PAYLOAD;
        // Boxed words have their top 16 bits in 0xFFF1..=0xFFFF, so the tag is 0..=14
        #[allow(clippy::cast_possible_truncation)]
        let tag = ((w >> ValueBits::TAG_SHIFT) - (ValueBits::BOXED_MIN >> ValueBits::TAG_SHIFT)) as u8;
        match tag {
            ValueTag::UNDEFINED => ValueKind::Undefined,
            ValueTag::NULL => ValueKind::Null,
            ValueTag::UNINIT => ValueKind::Uninit,
            ValueTag::BOOL => ValueKind::Bool(payload != 0),
            // SAFETY (all four): a pointer word was built from a live object's address (never 0), and while the value
            // is reachable the collector keeps that address current
            ValueTag::STRING => ValueKind::String(unsafe { Gc::from_addr_unchecked(Self::address(payload)) }),
            ValueTag::SYMBOL => ValueKind::Symbol(unsafe { Gc::from_addr_unchecked(Self::address(payload)) }),
            ValueTag::BIGINT => ValueKind::BigInt(unsafe { Gc::from_addr_unchecked(Self::address(payload)) }),
            ValueTag::OBJECT => ValueKind::Object(unsafe { Gc::from_addr_unchecked(Self::address(payload)) }),
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
        core::fmt::Debug::fmt(&self.kind(), f)
    }
}

/// The GC pointers inside are traced / moved through their kinds
unsafe impl<'gc> Collect<'gc> for Value<'gc> {
    /// Just a word
    const PARALLEL_DROP: bool = true;

    #[inline]
    fn trace(&self, t: &mut r8gc::Trace<'_>) {
        match self.kind() {
            ValueKind::String(s) => t.trace(&s),
            ValueKind::Symbol(s) => t.trace(&s),
            ValueKind::BigInt(b) => t.trace(&b),
            ValueKind::Object(o) => t.trace(&o),
            _ => {}
        }
    }

    #[inline]
    fn relocate(&mut self, r: &mut r8gc::Relocate<'_>) {
        *self = match self.kind() {
            ValueKind::String(mut s) => {
                r.relocate(&mut s);
                Value::String(s)
            }
            ValueKind::Symbol(mut s) => {
                r.relocate(&mut s);
                Value::Symbol(s)
            }
            ValueKind::BigInt(mut b) => {
                r.relocate(&mut b);
                Value::BigInt(b)
            }
            ValueKind::Object(mut o) => {
                r.relocate(&mut o);
                Value::Object(o)
            }
            _ => return,
        };
    }
}

/// The word itself: an atomic slot stores a value as is
impl r8gc::Word for Value<'_> {
    #[inline]
    fn into_word(self) -> u64 {
        self.bits
    }

    #[inline]
    fn from_word(w: u64) -> Self {
        Self::word(w)
    }
}

/// The tags of [`ValueKind`], and of boxed [`Value`] words (see [`ValueBits`]).
pub struct ValueTag;

impl ValueTag {
    pub const UNDEFINED: u8 = 0;
    pub const NULL: u8 = 1;
    pub const BOOL: u8 = 2;
    pub const NUMBER: u8 = 3;
    pub const STRING: u8 = 4;
    pub const SYMBOL: u8 = 5;
    pub const BIGINT: u8 = 6;
    pub const OBJECT: u8 = 7;
    pub const UNINIT: u8 = 8;
    /// The number of bytes a value takes
    pub const SIZE: usize = 8;
}

const _: () = assert!(core::mem::size_of::<Value<'static>>() == ValueTag::SIZE);

/// Strict equality (`===`): strings compare by content, objects by identity, and `NaN` is not equal to itself.
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
    /// Prototype (possibly absent) ⇒ value: `null` if absent.
    pub fn from_proto(p: Option<crate::JsObject<'gc>>) -> Self {
        p.map_or(Value::Null, Value::Object)
    }
}

impl Value<'_> {
    /// SameValue: like `===`, but `NaN` equals itself and `+0` does not equal `-0`.
    pub fn same_value(self, o: Self) -> bool {
        match (self.kind(), o.kind()) {
            (ValueKind::Number(a), ValueKind::Number(b)) => (a.is_nan() && b.is_nan()) || (a == b && a.is_sign_negative() == b.is_sign_negative()),
            _ => self == o,
        }
    }

    pub fn is_nullish(self) -> bool {
        matches!(self.kind(), ValueKind::Undefined | ValueKind::Null)
    }

    /// A primitive wrapper object ⇒ the primitive inside; anything else as is. The spec's thisBooleanValue /
    /// thisNumberValue / thisStringValue go through this step first.
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
    use r8gc::Word;

    use super::Value;

    /// Scalars survive the one-word form; every NaN comes back as a NaN; ±0 and ±∞ keep their sign
    #[test]
    fn word_round_trip() {
        for v in [Value::Undefined, Value::Null, Value::Uninit, Value::Bool(true), Value::Bool(false)] {
            assert!(Value::from_word(v.into_word()).same_value(v));
        }
        for n in [0.0, -0.0, 1.5, -2.0, f64::INFINITY, f64::NEG_INFINITY, f64::MAX, f64::MIN_POSITIVE, f64::NAN, -f64::NAN] {
            assert!(Value::from_word(Value::Number(n).into_word()).same_value(Value::Number(n)), "{n}");
        }
    }
}
