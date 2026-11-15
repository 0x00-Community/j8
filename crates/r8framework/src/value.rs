use r8gc::{Collect, Gc};

use crate::{JsBigInt, JsObject, JsString, JsSymbol};

/// A JS value: scalars are stored directly, strings and objects are GC pointers living in `'gc`.
///
/// The memory layout is fixed (`repr(C, u8)`, see [`ValueTag`]): JIT-generated machine code reads and writes values in
/// the register file directly.
#[derive(Clone, Copy, Debug, Default, Collect)]
#[collect(no_drop)]
#[repr(C, u8)]
pub enum Value<'gc> {
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

/// What a [`Value`] looks like in memory: [`SIZE`](Self::SIZE) bytes in total; byte 0 is one of the tags below, and the
/// payload (one byte for a boolean, the `f64` bits for a number, a GC pointer) starts at byte
/// [`PAYLOAD`](Self::PAYLOAD).
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
    pub const SIZE: usize = 16;
    /// The byte offset of the payload (`repr(C, u8)`: after the tag, aligned to the payload's largest alignment, 8
    /// bytes)
    pub const PAYLOAD: usize = 8;
}

const _: () = assert!(core::mem::size_of::<Value<'static>>() == ValueTag::SIZE);

/// The one-word (NaN-boxed) form of a [`Value`], so a value fits an atomic slot: `r8gc::Lock<Value>` is an atomic
/// value cell any thread may read and write.
///
/// Numbers are their own bits (every NaN becomes the canonical positive quiet NaN). Everything else lives in the
/// negative NaN space: the top 16 bits are `0xFFF0 | (tag + 1)` (never 0 there, so `-Infinity` = `0xFFF0…0` cannot
/// collide) and the low 48 bits carry the payload (a bool, or a GC address: user-space addresses fit in 48 bits).
pub struct ValueWord;

impl ValueWord {
    /// The canonical NaN every number NaN is stored as
    const CANONICAL_NAN: u64 = 0x7FF8_0000_0000_0000;
    /// Top 12 bits of every boxed value (a negative NaN)
    const BOXED: u64 = 0xFFF0_0000_0000_0000;
    /// Where the (tag + 1) nibble sits
    const TAG_SHIFT: u32 = 48;
    /// The (tag + 1) nibble
    const TAG_MASK: u64 = 0xF;
    /// The low 48 bits: the payload
    const PAYLOAD: u64 = (1 << Self::TAG_SHIFT) - 1;

    #[inline]
    fn boxed(tag: u8, payload: u64) -> u64 {
        debug_assert!(payload <= Self::PAYLOAD, "GC address beyond 48 bits");
        Self::BOXED | ((u64::from(tag) + 1) << Self::TAG_SHIFT) | payload
    }
}

impl r8gc::Word for Value<'_> {
    #[inline]
    fn into_word(self) -> u64 {
        match self {
            Value::Number(n) if n.is_nan() => ValueWord::CANONICAL_NAN,
            Value::Number(n) => n.to_bits(),
            Value::Undefined => ValueWord::boxed(ValueTag::UNDEFINED, 0),
            Value::Null => ValueWord::boxed(ValueTag::NULL, 0),
            Value::Uninit => ValueWord::boxed(ValueTag::UNINIT, 0),
            Value::Bool(b) => ValueWord::boxed(ValueTag::BOOL, u64::from(b)),
            Value::String(g) => ValueWord::boxed(ValueTag::STRING, g.into_word()),
            Value::Symbol(g) => ValueWord::boxed(ValueTag::SYMBOL, g.into_word()),
            Value::BigInt(g) => ValueWord::boxed(ValueTag::BIGINT, g.into_word()),
            Value::Object(g) => ValueWord::boxed(ValueTag::OBJECT, g.into_word()),
        }
    }

    #[inline]
    fn from_word(w: u64) -> Self {
        let nibble = (w >> ValueWord::TAG_SHIFT) & ValueWord::TAG_MASK;
        if w & ValueWord::BOXED != ValueWord::BOXED || nibble == 0 {
            return Value::Number(f64::from_bits(w));
        }
        let payload = w & ValueWord::PAYLOAD;
        // The nibble is 1..=15, so the tag fits a byte
        match u8::try_from(nibble - 1).unwrap_or(u8::MAX) {
            ValueTag::UNDEFINED => Value::Undefined,
            ValueTag::NULL => Value::Null,
            ValueTag::UNINIT => Value::Uninit,
            ValueTag::BOOL => Value::Bool(payload != 0),
            ValueTag::STRING => Value::String(Gc::from_word(payload)),
            ValueTag::SYMBOL => Value::Symbol(Gc::from_word(payload)),
            ValueTag::BIGINT => Value::BigInt(Gc::from_word(payload)),
            ValueTag::OBJECT => Value::Object(Gc::from_word(payload)),
            tag => unreachable!("boxed value with tag {tag}"),
        }
    }
}

/// Strict equality (`===`): strings compare by content, objects by identity, and `NaN` is not equal to itself.
impl PartialEq for Value<'_> {
    #[inline]
    fn eq(&self, o: &Self) -> bool {
        match (self, o) {
            (Value::Undefined, Value::Undefined) | (Value::Null, Value::Null) | (Value::Uninit, Value::Uninit) => true,
            (Value::Bool(a), Value::Bool(b)) => a == b,
            (Value::Number(a), Value::Number(b)) => a == b,
            (Value::String(a), Value::String(b)) => **a == **b,
            (Value::Object(a), Value::Object(b)) => Gc::ptr_eq(*a, *b),
            (Value::Symbol(a), Value::Symbol(b)) => Gc::ptr_eq(*a, *b),
            (Value::BigInt(a), Value::BigInt(b)) => a.0 == b.0,
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
        match (self, o) {
            (Value::Number(a), Value::Number(b)) => (a.is_nan() && b.is_nan()) || (a == b && a.is_sign_negative() == b.is_sign_negative()),
            _ => self == o,
        }
    }

    pub fn is_nullish(self) -> bool {
        matches!(self, Value::Undefined | Value::Null)
    }

    /// A primitive wrapper object ⇒ the primitive inside; anything else as is. The spec's thisBooleanValue /
    /// thisNumberValue / thisStringValue go through this step first.
    #[must_use]
    pub fn unwrapped(self) -> Self {
        match self {
            Value::Object(o) => match o.borrow().kind {
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
