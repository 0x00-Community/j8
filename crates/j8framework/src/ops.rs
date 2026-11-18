//! Operation semantics from the JS spec (shared by all execution layers): type conversion, operators, property access.
use crate::ValueKind;
use alloc::format;
use alloc::string::String;
use alloc::vec::Vec;

use crate::{Gc};
use j8bytecode::{BinaryOp, CompareOp, UnaryOp};

use num_traits::Zero;

use crate::{Agent, Descriptor, Error, Key, Numeric, IterKind, IteratorState, JsObject, JsString, JsSymbol, ObjectData, ObjectKind, Property, PropertyKey, Slot, Value};

/// Built-in error kinds (`Error` itself comes first).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum ErrorKind {
    Error,
    Type,
    Reference,
    Range,
    Syntax,
    Eval,
    Uri,
    Aggregate,
}

/// The preference for ToPrimitive.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Hint {
    Default,
    Number,
    String,
}

/// Spec semantics of binary operations and comparisons on two numbers (the operation enums are defined in
/// [`j8bytecode`]).
pub trait Numbers {
    type Output;
    fn numbers(self, a: f64, b: f64) -> Self::Output;
}

impl Numbers for BinaryOp {
    type Output = f64;

    /// The result when both sides are already numbers (the fast path of each execution layer, and the last step of
    /// [`Agent::binary`]). Always inlined: the interpreter's loop calls it for every arithmetic instruction, and a call
    /// plus a jump table on `self` cost more than `+` itself; the rare slow parts stay out of line.
    #[expect(clippy::inline_always, reason = "measured: left to LLVM it stays a call with a jump table in the interpreter loop")]
    #[inline(always)]
    fn numbers(self, a: f64, b: f64) -> f64 {
        match self {
            BinaryOp::Add => a + b,
            BinaryOp::Sub => a - b,
            BinaryOp::Mul => a * b,
            BinaryOp::Div => a / b,
            BinaryOp::Rem => remainder(a, b),
            BinaryOp::Exp => pow(a, b),
            BinaryOp::BitAnd => f64::from(to_int32(a) & to_int32(b)),
            BinaryOp::BitOr => f64::from(to_int32(a) | to_int32(b)),
            BinaryOp::BitXor => f64::from(to_int32(a) ^ to_int32(b)),
            BinaryOp::Shl => f64::from(to_int32(a).wrapping_shl(modulo_u32(b) & 31)),
            BinaryOp::Shr => f64::from(to_int32(a).wrapping_shr(modulo_u32(b) & 31)),
            BinaryOp::UShr => f64::from(modulo_u32(a) >> (modulo_u32(b) & 31)),
        }
    }
}

impl Numbers for CompareOp {
    type Output = bool;

    /// The result when both sides are numbers (`NaN` compares false with anything; `==` and `===` agree on numbers).
    #[inline]
    fn numbers(self, a: f64, b: f64) -> bool {
        match self {
            CompareOp::Eq | CompareOp::StrictEq => a == b,
            CompareOp::Ne | CompareOp::StrictNe => a != b,
            CompareOp::Lt => a < b,
            CompareOp::Gt => a > b,
            CompareOp::Le => a <= b,
            CompareOp::Ge => a >= b,
        }
    }
}

impl ErrorKind {
    pub const ALL: [ErrorKind; 8] = [ErrorKind::Error, ErrorKind::Type, ErrorKind::Reference, ErrorKind::Range, ErrorKind::Syntax, ErrorKind::Eval, ErrorKind::Uri, ErrorKind::Aggregate];

    pub fn name(self) -> &'static str {
        match self {
            ErrorKind::Error => "Error",
            ErrorKind::Type => "TypeError",
            ErrorKind::Reference => "ReferenceError",
            ErrorKind::Range => "RangeError",
            ErrorKind::Syntax => "SyntaxError",
            ErrorKind::Eval => "EvalError",
            ErrorKind::Uri => "URIError",
            ErrorKind::Aggregate => "AggregateError",
        }
    }
}

/// An iterator record: the iterator object and its `next` method.
#[derive(Clone, Copy, Debug, crate::Collect)]
#[collect(no_drop)]
pub struct IterRecord<'gc> {
    pub iterator: Value<'gc>,
    pub next: Value<'gc>,
}

/// 2^32: the modulus of ToInt32 / ToUint32.
const TWO_POW_32: f64 = 4_294_967_296.0;

/// Upper bound of array indices (`length` is at most 2^32 − 1).
const MAX_INDEX: f64 = 4_294_967_294.0;

/// String ⇒ number (StringToNumber): leading / trailing whitespace, `Infinity`, `0x` / `0o` / `0b` prefixes, decimal.
pub(crate) fn parse_number(s: &str) -> f64 {
    let t = s.trim_matches(|c: char| (c.is_whitespace() && c != '\u{85}') || c == '\u{feff}');
    match t {
        "" => 0.0,
        "Infinity" | "+Infinity" => f64::INFINITY,
        "-Infinity" => f64::NEG_INFINITY,
        _ => {
            let radix = match t.get(..2) {
                Some("0x" | "0X") => 16,
                Some("0o" | "0O") => 8,
                Some("0b" | "0B") => 2,
                _ => 10,
            };
            if radix == 10 {
                let ok = t.bytes().all(|c| c.is_ascii_digit() || matches!(c, b'+' | b'-' | b'.' | b'e' | b'E'));
                return if ok { t.parse().unwrap_or(f64::NAN) } else { f64::NAN };
            }
            let digits = &t[2..];
            if digits.is_empty() {
                return f64::NAN;
            }
            digits.chars().try_fold(0.0, |acc, c| c.to_digit(radix).map(|d| acc * f64::from(radix) + f64::from(d))).unwrap_or(f64::NAN)
        }
    }
}

/// Number ⇒ the remainder of a 32-bit integer modulo 2^32 (shared by ToInt32 / ToUint32).
/// 2^53 − 1
pub const MAX_SAFE_INTEGER: f64 = 9_007_199_254_740_991.0;

#[inline]
fn modulo_u32(x: f64) -> u32 {
    // Fast path: already an integer in the i32 / u32 range (the operand of most bitwise operations); truncate directly
    #[expect(clippy::cast_possible_truncation, reason = "the comparison below confirms nothing was truncated")]
    let i = x as i32;
    if f64::from(i) == x {
        return i.cast_unsigned();
    }
    #[expect(clippy::cast_possible_truncation, clippy::cast_sign_loss, reason = "the comparison below confirms nothing was truncated")]
    let u = x as u32;
    if f64::from(u) == x {
        return u;
    }
    modulo_u32_slow(x)
}

#[cold]
#[inline(never)]
fn modulo_u32_slow(x: f64) -> u32 {
    if !x.is_finite() {
        return 0;
    }
    let m = libm::trunc(x) % TWO_POW_32;
    let m = if m < 0.0 { m + TWO_POW_32 } else { m };
    // m is an integer in [0, 2^32)
    #[expect(clippy::cast_possible_truncation, clippy::cast_sign_loss, reason = "m is already reduced to [0, 2^32)")]
    let u = m as u32;
    u
}

/// Number::toString(x): safe integers are written in decimal straight into a stack buffer (short, so it becomes a
/// [`SmallStr`](crate::SmallStr) without allocating); everything else goes to ryu-js.
pub fn number_text(x: f64) -> crate::SmallStr {
    #[expect(clippy::cast_possible_truncation, reason = "the comparison below confirms it is a safe integer")]
    let i = x as i64;
    #[expect(clippy::cast_precision_loss, reason = "exact within the safe integer range")]
    if x.abs() <= MAX_SAFE_INTEGER && i as f64 == x {
        // Longest is "-9007199254740991"
        let mut buf = [0u8; 20];
        let mut at = buf.len();
        let mut n = i.unsigned_abs();
        loop {
            at -= 1;
            let digit = (n % 10) as u8;
            buf[at] = b'0' + digit;
            n /= 10;
            if n == 0 {
                break;
            }
        }
        if i < 0 {
            at -= 1;
            buf[at] = b'-';
        }
        return crate::SmallStr::from(core::str::from_utf8(&buf[at..]).unwrap_or_default());
    }
    crate::SmallStr::from(ryu_js::Buffer::new().format(x))
}

/// ToInt32
#[inline]
fn to_int32(x: f64) -> i32 {
    modulo_u32(x).cast_signed()
}

/// `%`: when both sides are safe integers use integer remainder (a zero result takes the dividend's sign), otherwise
/// use floating-point `fmod`.
#[inline]
fn remainder(a: f64, b: f64) -> f64 {
    if a.abs() <= MAX_SAFE_INTEGER && b.abs() <= MAX_SAFE_INTEGER && b != 0.0 {
        #[expect(clippy::cast_possible_truncation, reason = "checked to be in the safe integer range; the comparison below confirms it is an integer")]
        let (x, y) = (a as i64, b as i64);
        #[expect(clippy::cast_precision_loss, reason = "exact within the safe integer range")]
        if x as f64 == a && y as f64 == b {
            #[expect(clippy::cast_precision_loss, reason = "the remainder's magnitude is below the divisor's; exact")]
            let r = (x % y) as f64;
            return if r == 0.0 { 0.0f64.copysign(a) } else { r };
        }
    }
    fmod(a, b)
}

/// `%` outside the safe integers: floating-point `fmod`.
#[cold]
#[inline(never)]
fn fmod(a: f64, b: f64) -> f64 {
    a % b
}

/// `**`: where JS differs from C `pow` is that `±1 ** ±∞` and `x ** NaN` are both NaN.
#[inline(never)]
fn pow(a: f64, b: f64) -> f64 {
    if b.is_nan() || ((a == 1.0 || a == -1.0) && b.is_infinite()) { f64::NAN } else { libm::pow(a, b) }
}

/// Canonical index: a non-negative integer less than 2^32 − 1.
fn index_of_number(x: f64) -> Option<u32> {
    // Conversion to u32 saturates (negative and NaN give 0, too large gives u32::MAX); equal after converting back
    // means an integer; -0 counts as 0
    #[expect(clippy::cast_possible_truncation, clippy::cast_sign_loss, reason = "converted back and checked for equality")]
    let n = x as u32;
    (f64::from(n) == x && x <= MAX_INDEX).then_some(n)
}

/// The string form of a canonical index (`"0"`, `"12"`, no leading zeros).
pub(crate) fn index_of_str(s: &str) -> Option<u32> {
    let canonical = s == "0" || (!s.starts_with('0') && !s.is_empty() && s.bytes().all(|c| c.is_ascii_digit()));
    canonical.then(|| s.parse().ok()).flatten().filter(|&i| f64::from(i) <= MAX_INDEX)
}

impl<'gc> Agent<'gc, '_> {
    /// Put a string on the heap.
    /// Borrow the platform's thread pool for parallel work (only for plain data that doesn't touch the GC heap)
    pub fn par(&self) -> crate::Par<'_> {
        crate::Par(self.platform)
    }

    /// A string of one UTF-16 code unit (ASCII uses the prebuilt ones)
    pub fn unit_string(&self, u: u16) -> JsString<'gc> {
        match self.realm.chars.get(usize::from(u)) {
            Some(&s) => s,
            None => self.string(&alloc::string::String::from_utf16_lossy(&[u])),
        }
    }

    pub fn string(&self, s: &str) -> JsString<'gc> {
        self.mc.alloc(crate::JsStr::from(s))
    }

    /// An empty object with `Object.prototype` as its prototype.
    pub fn object(&self) -> JsObject<'gc> {
        ObjectData { proto: Some(self.realm.object_proto), ..ObjectData::default() }.alloc(self.mc)
    }

    /// Same as [`object`](Self::object), reserving slots for `capacity` properties (literals)
    pub fn object_with(&self, capacity: usize) -> JsObject<'gc> {
        ObjectData::ordinary(self.mc, Some(self.realm.object_proto), capacity)
    }

    /// An array with `Array.prototype` as its prototype.
    pub fn array(&self, items: Vec<Value<'gc>>) -> JsObject<'gc> {
        ObjectData { kind: ObjectKind::Array, elements: items, proto: Some(self.realm.array_proto), ..ObjectData::default() }.alloc(self.mc)
    }

    /// An Error object (prototype chosen by kind, with an own `message`).
    pub fn error(&self, kind: ErrorKind, message: &str) -> JsObject<'gc> {
        let e = ObjectData { kind: ObjectKind::Error, proto: Some(self.realm.error_proto(kind)), ..ObjectData::default() }.alloc(self.mc);
        let m = self.string(message);
        e.borrow_mut(self.mc).define("message", Property::hidden(m.into()));
        e
    }

    /// Throw a built-in error.
    pub fn throw(&self, kind: ErrorKind, message: &str) -> Error {
        Error::Exception(self.global(self.error(kind, message).into()))
    }

    /// Throw an arbitrary value.
    pub fn throw_value(&self, v: Value<'gc>) -> Error {
        Error::Exception(self.global(v))
    }

    pub fn is_callable(&self, v: Value<'gc>) -> bool {
        matches!(v.kind(), ValueKind::Object(o) if o.borrow().kind.is_callable())
    }

    #[inline]
    pub fn to_boolean(&self, v: Value<'gc>) -> bool {
        match v.kind() {
            ValueKind::Undefined | ValueKind::Null | ValueKind::Uninit => false,
            ValueKind::Bool(b) => b,
            ValueKind::Number(x) => !(x == 0.0 || x.is_nan()),
            // Judge by byte count, without flattening the rope
            ValueKind::String(s) => s.text().byte_len() != 0,
            ValueKind::BigInt(b) => !b.0.is_zero(),
            ValueKind::Symbol(_) | ValueKind::Object(_) => true,
        }
    }

    /// ToPrimitive: for objects, try `valueOf` / `toString` in preference order and use the first primitive result.
    pub fn to_primitive(&mut self, v: Value<'gc>, hint: Hint) -> Result<Value<'gc>, Error> {
        if v.as_object().is_none() {
            return Ok(v);
        }
        let exotic = self.get_symbol(v, self.realm.symbols.to_primitive, v)?;
        if !exotic.is_nullish() {
            let h = self.string(match hint {
                Hint::Default => "default",
                Hint::Number => "number",
                Hint::String => "string",
            });
            let r = self.call(exotic, v, &[h.into()])?;
            if r.as_object().is_some() {
                return Err(self.throw(ErrorKind::Type, "Cannot convert object to primitive value"));
            }
            return Ok(r);
        }
        let order = if hint == Hint::String { ["toString", "valueOf"] } else { ["valueOf", "toString"] };
        for name in order {
            let f = self.get_named(v, name)?;
            if self.is_callable(f) {
                let r = self.call(f, v, &[])?;
                if r.as_object().is_none() {
                    return Ok(r);
                }
            }
        }
        Err(self.throw(ErrorKind::Type, "Cannot convert object to primitive value"))
    }

    /// ToNumber.
    pub fn to_number(&mut self, v: Value<'gc>) -> Result<f64, Error> {
        Ok(match v.kind() {
            ValueKind::Undefined | ValueKind::Uninit => f64::NAN,
            ValueKind::Null => 0.0,
            ValueKind::Bool(b) => f64::from(u8::from(b)),
            ValueKind::Number(x) => x,
            ValueKind::String(s) => parse_number(&s),
            ValueKind::Symbol(_) => return Err(self.throw(ErrorKind::Type, "Cannot convert a Symbol value to a number")),
            ValueKind::BigInt(_) => return Err(self.throw(ErrorKind::Type, "Cannot convert a BigInt value to a number")),
            ValueKind::Object(_) => {
                let p = self.to_primitive(v, Hint::Number)?;
                return self.to_number(p);
            }
        })
    }

    pub fn to_int32(&mut self, v: Value<'gc>) -> Result<i32, Error> {
        Ok(modulo_u32(self.to_number(v)?).cast_signed())
    }

    pub fn to_uint32(&mut self, v: Value<'gc>) -> Result<u32, Error> {
        Ok(modulo_u32(self.to_number(v)?))
    }

    /// ToString.
    pub fn to_string(&mut self, v: Value<'gc>) -> Result<JsString<'gc>, Error> {
        Ok(match v.kind() {
            ValueKind::String(s) => s,
            ValueKind::Undefined | ValueKind::Uninit => self.string("undefined"),
            ValueKind::Null => self.string("null"),
            ValueKind::Bool(true) => self.string("true"),
            ValueKind::Bool(false) => self.string("false"),
            ValueKind::Number(x) => self.mc.alloc(crate::JsStr::from(number_text(x))),
            ValueKind::Symbol(_) => return Err(self.throw(ErrorKind::Type, "Cannot convert a Symbol value to a string")),
            ValueKind::BigInt(b) => self.string(&format!("{}", b.0)),
            ValueKind::Object(_) => {
                let p = self.to_primitive(v, Hint::String)?;
                return self.to_string(p);
            }
        })
    }

    /// A Symbol in `String(sym)` / error messages: `Symbol(description)`.
    pub fn symbol_descriptive(&self, s: JsSymbol<'gc>) -> JsString<'gc> {
        self.string(&format!("Symbol({})", s.description.as_deref().unwrap_or("")))
    }

    pub fn type_of(&self, v: Value<'gc>) -> &'static str {
        match v.kind() {
            ValueKind::Undefined | ValueKind::Uninit => "undefined",
            ValueKind::Bool(_) => "boolean",
            ValueKind::Number(_) => "number",
            ValueKind::String(_) => "string",
            ValueKind::Symbol(_) => "symbol",
            ValueKind::BigInt(_) => "bigint",
            ValueKind::Object(o) if o.borrow().kind.is_callable() => "function",
            ValueKind::Null | ValueKind::Object(_) => "object",
        }
    }

    pub fn binary(&mut self, op: BinaryOp, lhs: Value<'gc>, rhs: Value<'gc>) -> Result<Value<'gc>, Error> {
        if let (ValueKind::Number(a), ValueKind::Number(b)) = (lhs.kind(), rhs.kind()) {
            return Ok(op.numbers(a, b).into());
        }
        // Addition: ToPrimitive both sides first to see whether either is a string; other operations finish ToNumeric
        // on the whole left side before the right (the evaluation order is observable)
        let (l, r) = if op == BinaryOp::Add {
            let (l, r) = (self.to_primitive(lhs, Hint::Default)?, self.to_primitive(rhs, Hint::Default)?);
            if matches!(l.kind(), ValueKind::String(_)) || matches!(r.kind(), ValueKind::String(_)) {
                let (a, b) = (self.to_string(l)?, self.to_string(r)?);
                return Ok(Value::String(self.mc.alloc(crate::JsStr::from(crate::SmallStr::concat(a.text(), b.text())))));
            }
            (l, r)
        } else {
            (lhs, rhs)
        };
        let ln = self.to_numeric(l)?;
        let rn = self.to_numeric(r)?;
        let (a, b) = match (ln, rn) {
            (Numeric::Number(a), Numeric::Number(b)) => (a, b),
            (Numeric::BigInt(a), Numeric::BigInt(b)) => return self.bigint_binary(op, &a.0, &b.0),
            _ => return Err(self.throw(ErrorKind::Type, "Cannot mix BigInt and other types, use explicit conversions")),
        };
        Ok(op.numbers(a, b).into())
    }

    /// Loose equality (`==`).
    pub fn loose_equals(&mut self, a: Value<'gc>, b: Value<'gc>) -> Result<bool, Error> {
        Ok(match (a.kind(), b.kind()) {
            _ if a.is_nullish() && b.is_nullish() => true,
            (ValueKind::Undefined | ValueKind::Null, _) | (_, ValueKind::Undefined | ValueKind::Null) => false,
            (ValueKind::Object(_), ValueKind::Object(_)) | (ValueKind::Symbol(_), ValueKind::Symbol(_)) => a == b,
            (ValueKind::Object(_), _) => {
                let p = self.to_primitive(a, Hint::Default)?;
                return self.loose_equals(p, b);
            }
            (_, ValueKind::Object(_)) => {
                let p = self.to_primitive(b, Hint::Default)?;
                return self.loose_equals(a, p);
            }
            (ValueKind::Symbol(_), _) | (_, ValueKind::Symbol(_)) => false,
            (ValueKind::BigInt(x), ValueKind::BigInt(y)) => x.0 == y.0,
            (ValueKind::BigInt(x), ValueKind::String(s)) | (ValueKind::String(s), ValueKind::BigInt(x)) => Self::parse_bigint(&s).is_some_and(|y| x.0 == y),
            (ValueKind::BigInt(x), _) | (_, ValueKind::BigInt(x)) => {
                let other = if matches!(a.kind(), ValueKind::BigInt(_)) { b } else { a };
                let n = self.to_number(other)?;
                Self::cmp_bigint_number(&x.0, n) == Some(core::cmp::Ordering::Equal)
            }
            (ValueKind::Number(_), ValueKind::String(_)) | (ValueKind::String(_), ValueKind::Number(_)) | (ValueKind::Bool(_), _) | (_, ValueKind::Bool(_)) => self.to_number(a)? == self.to_number(b)?,
            _ => a == b,
        })
    }

    pub fn compare(&mut self, op: CompareOp, lhs: Value<'gc>, rhs: Value<'gc>) -> Result<bool, Error> {
        if let (ValueKind::Number(a), ValueKind::Number(b)) = (lhs.kind(), rhs.kind()) {
            return Ok(op.numbers(a, b));
        }
        Ok(match op {
            CompareOp::Eq => self.loose_equals(lhs, rhs)?,
            CompareOp::Ne => !self.loose_equals(lhs, rhs)?,
            CompareOp::StrictEq => lhs == rhs,
            CompareOp::StrictNe => lhs != rhs,
            CompareOp::Lt | CompareOp::Gt | CompareOp::Le | CompareOp::Ge => {
                let (l, r) = (self.to_primitive(lhs, Hint::Number)?, self.to_primitive(rhs, Hint::Number)?);
                if let (ValueKind::String(a), ValueKind::String(b)) = (l.kind(), r.kind()) {
                    // Compare by UTF-16 code unit
                    let ord = a.encode_utf16().cmp(b.encode_utf16());
                    return Ok(match op {
                        CompareOp::Lt => ord.is_lt(),
                        CompareOp::Gt => ord.is_gt(),
                        CompareOp::Le => ord.is_le(),
                        _ => ord.is_ge(),
                    });
                }
                let Some(ord) = self.numeric_order(l, r)? else { return Ok(false) };
                match op {
                    CompareOp::Lt => ord.is_lt(),
                    CompareOp::Gt => ord.is_gt(),
                    CompareOp::Le => ord.is_le(),
                    _ => ord.is_ge(),
                }
            }
        })
    }

    pub fn unary(&mut self, op: UnaryOp, v: Value<'gc>) -> Result<Value<'gc>, Error> {
        Ok(match op {
            UnaryOp::Neg => match self.to_numeric(v)? {
                Numeric::Number(x) => (-x).into(),
                Numeric::BigInt(b) => self.bigint(-&b.0),
            },
            UnaryOp::Plus => self.to_number(v)?.into(),
            UnaryOp::Not => (!self.to_boolean(v)).into(),
            UnaryOp::BitNot => match self.to_numeric(v)? {
                Numeric::Number(x) => f64::from(!modulo_u32(x).cast_signed()).into(),
                Numeric::BigInt(b) => self.bigint(!&b.0),
            },
            UnaryOp::Typeof => self.string(self.type_of(v)).into(),
            UnaryOp::Void => Value::Undefined,
        })
    }

    /// ToObject: objects as is, primitives wrapped in wrapper objects (whose prototype is that kind's prototype);
    /// `undefined` / `null` throw a TypeError.
    pub fn to_object(&self, v: Value<'gc>) -> Result<JsObject<'gc>, Error> {
        match (v.kind(), self.proto_of(v)) {
            (ValueKind::Object(o), _) => Ok(o),
            (_, Some(proto)) => Ok(ObjectData { kind: ObjectKind::Primitive(v), proto: Some(proto), ..ObjectData::default() }.alloc(self.mc)),
            (_, None) => Err(self.throw(ErrorKind::Type, "Cannot convert undefined or null to object")),
        }
    }

    /// The prototype used when reading a property of a primitive.
    fn proto_of(&self, v: Value<'gc>) -> Option<JsObject<'gc>> {
        match v.kind() {
            ValueKind::String(_) => Some(self.realm.string_proto),
            ValueKind::Number(_) => Some(self.realm.number_proto),
            ValueKind::Bool(_) => Some(self.realm.boolean_proto),
            ValueKind::Symbol(_) => Some(self.realm.symbol_proto),
            ValueKind::BigInt(_) => Some(self.realm.bigint_proto),
            ValueKind::Object(o) => Some(o),
            ValueKind::Undefined | ValueKind::Null | ValueKind::Uninit => None,
        }
    }

    fn nullish_error(&self, target: Value<'gc>, verb: &str, key: &str) -> Error {
        let what = if target == Value::Undefined { "undefined" } else { "null" };
        self.throw(ErrorKind::Type, &format!("Cannot {verb} properties of {what} (reading '{key}')"))
    }

    /// [[Get]]: read `key` on `holder` (along the prototype chain); a getter's `this` is `receiver`.
    pub fn get_key(&mut self, holder: Value<'gc>, key: Key<'_, 'gc>, receiver: Value<'gc>) -> Result<Value<'gc>, Error> {
        if holder.is_nullish() {
            let k = self.key_text(key);
            return Err(self.nullish_error(holder, "read", &k));
        }
        // String indices and `length` don't need a wrapper
        if let (ValueKind::String(s), Key::Str(k)) = (holder.kind(), key) {
            if k == "length" {
                return Ok(f64::from(u32::try_from(s.utf16_len()).unwrap_or(u32::MAX)).into());
            }
            if let Some(i) = index_of_str(k)
                && let Some(u) = s.code_unit(i as usize)
            {
                return Ok(self.unit_string(u).into());
            }
        }
        match self.proto_of(holder) {
            Some(o) => self.object_get(o, key, receiver),
            None => Ok(Value::Undefined),
        }
    }

    /// [[Get]] of an object: search along the prototype chain (handing off to a Proxy if one is met).
    pub fn object_get(&mut self, o: JsObject<'gc>, key: Key<'_, 'gc>, receiver: Value<'gc>) -> Result<Value<'gc>, Error> {
        let mut cur = o;
        loop {
            // Fast path: an ordinary object checks its own properties in one borrow, and goes straight to the prototype
            // if not found
            let fast = cur.has_ordinary_properties().then(|| (cur.own_slot(key), cur.proto()));
            match fast {
                Some((Some(p), _)) => {
                    return match p.slot {
                        Slot::Data(v) => Ok(v),
                        Slot::Accessor { get: Some(g), .. } => self.call(g.into(), receiver, &[]),
                        Slot::Accessor { get: None, .. } => Ok(Value::Undefined),
                    };
                }
                Some((None, Some(proto))) => {
                    cur = proto;
                    continue;
                }
                Some((None, None)) => return Ok(Value::Undefined),
                None => {}
            }
            if let Some(parts) = self.proxy_parts(cur) {
                let (t, h) = parts?;
                return self.proxy_get(t, h, key, receiver);
            }
            if let Some(i) = Self::typed_numeric(cur, key) {
                return Ok(self.typed_get(cur, i).unwrap_or_default());
            }
            if let (Some(b), Key::Str(k)) = (Self::namespace_export(cur, key), key) {
                return match b {
                    Some(b) => self.binding_value(b, k),
                    None => Ok(Value::Undefined),
                };
            }
            if let Some(p) = self.get_own_property(cur, key)? {
                return match p.slot {
                    Slot::Data(v) => Ok(v),
                    Slot::Accessor { get: Some(g), .. } => self.call(g.into(), receiver, &[]),
                    Slot::Accessor { get: None, .. } => Ok(Value::Undefined),
                };
            }
            match self.get_prototype_of(cur)? {
                Some(n) => cur = n,
                None => return Ok(Value::Undefined),
            }
        }
    }

    /// Read `key` on `holder` (along the prototype chain); a getter's `this` is `receiver`.
    pub fn get(&mut self, holder: Value<'gc>, key: &str, receiver: Value<'gc>) -> Result<Value<'gc>, Error> {
        self.get_key(holder, Key::Str(key), receiver)
    }

    /// `target.key`.
    pub fn get_named(&mut self, target: Value<'gc>, key: &str) -> Result<Value<'gc>, Error> {
        self.get_key(target, Key::Str(key), target)
    }

    /// Property key: ToString (the number fast path for array indices is at the call site).
    pub fn property_key(&mut self, key: Value<'gc>) -> Result<JsString<'gc>, Error> {
        self.to_string(key)
    }

    /// `holder[sym]`; a getter's `this` is `receiver`.
    pub fn get_symbol(&mut self, holder: Value<'gc>, key: JsSymbol<'gc>, receiver: Value<'gc>) -> Result<Value<'gc>, Error> {
        self.get_key(holder, Key::Sym(key), receiver)
    }

    /// [[Set]] (OrdinarySet): call a setter found along the prototype chain, don't write a non-writable data property,
    /// otherwise update / create on `receiver`. Returns whether the write succeeded.
    pub fn set_key(&mut self, target: Value<'gc>, key: Key<'_, 'gc>, value: Value<'gc>, receiver: Value<'gc>) -> Result<bool, Error> {
        if target.is_nullish() {
            let k = self.key_text(key);
            return Err(self.nullish_error(target, "set", &k));
        }
        match self.proto_of(target) {
            Some(o) => self.object_set(o, key, value, receiver),
            None => Ok(false),
        }
    }

    /// [[Set]] of an object (OrdinarySet): call a setter found along the prototype chain, don't write a non-writable
    /// data property, otherwise update / create on `receiver`; hand off to a Proxy if one is met.
    pub fn object_set(&mut self, o: JsObject<'gc>, key: Key<'_, 'gc>, value: Value<'gc>, receiver: Value<'gc>) -> Result<bool, Error> {
        let mut cur = o;
        let found = loop {
            // Fast path: an ordinary object checks its own properties in one borrow, and goes straight to the prototype
            // if not found
            let fast = cur.has_ordinary_properties().then(|| (cur.own_slot(key), cur.proto()));
            match fast {
                Some((Some(p), _)) => break Some(p),
                Some((None, Some(proto))) => {
                    cur = proto;
                    continue;
                }
                Some((None, None)) => break None,
                None => {}
            }
            if let Some(parts) = self.proxy_parts(cur) {
                let (t, h) = parts?;
                return self.proxy_set(t, h, key, value, receiver);
            }
            if matches!(cur.borrow().kind, ObjectKind::ModuleNamespace(_)) {
                return Ok(false);
            }
            if let Some(i) = Self::typed_numeric(cur, key) {
                if matches!(receiver.kind(), ValueKind::Object(r) if Gc::ptr_eq(r, cur)) {
                    self.typed_set(cur, i, value)?;
                    return Ok(true);
                }
                if !Self::typed_has(cur, i) {
                    return Ok(true);
                }
            }
            if let Some(p) = self.get_own_property(cur, key)? {
                break Some(p);
            }
            match self.get_prototype_of(cur)? {
                Some(n) => cur = n,
                None => break None,
            }
        };
        match found {
            Some(Property { slot: Slot::Accessor { set: Some(s), .. }, .. }) => {
                self.call(s.into(), receiver, &[value])?;
                return Ok(true);
            }
            Some(Property { slot: Slot::Accessor { set: None, .. }, .. } | Property { writable: false, .. }) => return Ok(false),
            _ => {}
        }
        let Some(r) = receiver.as_object() else { return Ok(false) };
        if let Some(done) = r.borrow_mut(self.mc).ordinary_put(key, value) {
            return Ok(done);
        }
        match self.get_own_property(r, key)? {
            Some(Property { slot: Slot::Accessor { .. }, .. } | Property { writable: false, .. }) => Ok(false),
            Some(_) => self.define_own_property(r, key, Descriptor::value(value)),
            None => self.create_data_property(r, key, value),
        }
    }

    /// `target[sym] = value`; returns whether the write succeeded.
    pub fn try_set_symbol(&mut self, target: Value<'gc>, key: JsSymbol<'gc>, value: Value<'gc>) -> Result<bool, Error> {
        self.set_key(target, Key::Sym(key), value, target)
    }

    /// `target[sym] = value`: throws a TypeError if the write fails (Set(…, true) of built-in functions in the spec).
    pub fn set_symbol(&mut self, target: Value<'gc>, key: JsSymbol<'gc>, value: Value<'gc>) -> Result<(), Error> {
        if self.try_set_symbol(target, key, value)? {
            Ok(())
        } else {
            let what = self.symbol_descriptive(key);
            Err(self.read_only(&what))
        }
    }

    /// The TypeError for a failed write: adding a new property to a non-extensible object, or writing a read-only
    /// property.
    fn set_failure(&self, target: Value<'gc>, key: &str) -> Error {
        if let Some(o) = target.as_object() {
            let d = o.borrow();
            if d.non_extensible() && d.own_slot(Key::Str(key)).is_none() {
                return self.throw(ErrorKind::Type, &format!("Cannot add property {key}, object is not extensible"));
            }
        }
        self.read_only(key)
    }

    fn read_only(&self, key: &str) -> Error {
        self.throw(ErrorKind::Type, &format!("Cannot assign to read only property '{key}'"))
    }

    /// `key in target`, with an arbitrary value as the key.
    pub fn has_keyed(&mut self, target: Value<'gc>, key: Value<'gc>) -> Result<bool, Error> {
        let Some(o) = target.as_object() else {
            let what = match key.kind() {
                ValueKind::Symbol(_) => String::from("a symbol"),
                k => format!("'{}'", &**self.to_string(k.into())?),
            };
            let t = self.to_string(target)?;
            return Err(self.throw(ErrorKind::Type, &format!("Cannot use 'in' operator to search for {what} in {}", &**t)));
        };
        match self.to_property_key(key)? {
            PropertyKey::Symbol(k) => self.has_key(o, Key::Sym(k)),
            PropertyKey::String(k) => self.has_key(o, Key::Str(&k)),
        }
    }

    /// [[HasProperty]]: present as an own property or on the prototype chain.
    pub fn has_key(&mut self, o: JsObject<'gc>, key: Key<'_, 'gc>) -> Result<bool, Error> {
        let mut cur = o;
        loop {
            if let Some(parts) = self.proxy_parts(cur) {
                let (t, h) = parts?;
                return self.proxy_has(t, h, key);
            }
            if let Some(i) = Self::typed_numeric(cur, key) {
                return Ok(Self::typed_has(cur, i));
            }
            if let Some(b) = Self::namespace_export(cur, key) {
                return Ok(b.is_some());
            }
            if self.get_own_property(cur, key)?.is_some() {
                return Ok(true);
            }
            match self.get_prototype_of(cur)? {
                Some(n) => cur = n,
                None => return Ok(false),
            }
        }
    }

    /// `delete target[key]`, with an arbitrary value as the key.
    pub fn delete_keyed(&mut self, target: Value<'gc>, key: Value<'gc>) -> Result<bool, Error> {
        if target.is_nullish() {
            let k = self.to_string(key)?;
            return Err(self.nullish_error(target, "delete", &k));
        }
        match self.to_property_key(key)? {
            PropertyKey::Symbol(k) => self.delete_key(target, Key::Sym(k)),
            PropertyKey::String(k) => self.delete_key(target, Key::Str(&k)),
        }
    }

    /// `delete target.key`: primitives are wrapped in an object first (string indices and `length` can't be deleted).
    pub fn delete_key(&mut self, target: Value<'gc>, key: Key<'_, 'gc>) -> Result<bool, Error> {
        if target.is_nullish() {
            let k = self.key_text(key);
            return Err(self.nullish_error(target, "delete", &k));
        }
        let o = self.to_object(target)?;
        self.delete_property(o, key)
    }

    /// Define a property, with an arbitrary value as the key (computed key).
    pub fn define_keyed(&mut self, o: JsObject<'gc>, key: Value<'gc>, prop: Property<'gc>) -> Result<(), Error> {
        match self.to_property_key(key)? {
            PropertyKey::Symbol(k) => self.define_key(o, Key::Sym(k), prop),
            PropertyKey::String(k) => self.define_key(o, Key::Str(&k), prop),
        }
    }

    /// Define half of an accessor, with an arbitrary value as the key.
    pub fn define_accessor_keyed(&mut self, o: JsObject<'gc>, key: Value<'gc>, f: JsObject<'gc>, setter: bool, enumerable: bool) -> Result<(), Error> {
        match self.to_property_key(key)? {
            PropertyKey::Symbol(k) => self.define_accessor_key(o, Key::Sym(k), f, setter, enumerable),
            PropertyKey::String(k) => self.define_accessor_key(o, Key::Str(&k), f, setter, enumerable),
        }
        Ok(())
    }

    /// `target[key]`.
    pub fn get_keyed(&mut self, target: Value<'gc>, key: Value<'gc>) -> Result<Value<'gc>, Error> {
        if let ValueKind::Symbol(k) = key.kind() {
            return self.get_symbol(target, k, target);
        }
        if let (ValueKind::Object(o), ValueKind::Number(x)) = (target.kind(), key.kind())
            && o.is_array()
            && let Some(i) = index_of_number(x)
            && let Some(v) = o.elements().get(i as usize)
        {
            return Ok(v);
        }
        // Numeric keys of typed arrays: take the element directly, without looking at the prototype chain (key -0
        // converts to the string "0")
        if let (ValueKind::Object(o), ValueKind::Number(x)) = (target.kind(), key.kind())
            && matches!(o.borrow().kind, ObjectKind::TypedArray(..))
        {
            return Ok(self.typed_get(o, x + 0.0).unwrap_or_default());
        }
        // String indices: UTF-16 code units, O(1)
        if let (ValueKind::String(s), ValueKind::Number(x)) = (target.kind(), key.kind())
            && let Some(u) = index_of_number(x).and_then(|i| s.code_unit(i as usize))
        {
            return Ok(self.unit_string(u).into());
        }
        if target.is_nullish() {
            let k = self.to_string(key)?;
            return Err(self.nullish_error(target, "read", &k));
        }
        match self.to_property_key(key)? {
            PropertyKey::Symbol(k) => self.get_symbol(target, k, target),
            PropertyKey::String(k) => self.get_named(target, &k),
        }
    }

    /// `target.key = value`: returns whether the write succeeded.
    pub fn try_set_named(&mut self, target: Value<'gc>, key: &str, value: Value<'gc>) -> Result<bool, Error> {
        if let (ValueKind::Object(o), Some(i)) = (target.kind(), index_of_str(key))
            && o.borrow_mut(self.mc).write_index(i, value)
        {
            return Ok(true);
        }
        self.set_key(target, Key::Str(key), value, target)
    }

    /// `target.key = value`: throws a TypeError if the write fails (Set(…, true) of built-in functions in the spec).
    pub fn set_named(&mut self, target: Value<'gc>, key: &str, value: Value<'gc>) -> Result<(), Error> {
        if self.try_set_named(target, key, value)? { Ok(()) } else { Err(self.set_failure(target, key)) }
    }

    /// `target[key] = value`; returns whether the write succeeded.
    pub fn try_set_keyed(&mut self, target: Value<'gc>, key: Value<'gc>, value: Value<'gc>) -> Result<bool, Error> {
        if let ValueKind::Symbol(k) = key.kind() {
            return self.try_set_symbol(target, k, value);
        }
        if let (ValueKind::Object(o), ValueKind::Number(x)) = (target.kind(), key.kind())
            && let Some(i) = index_of_number(x)
            && o.borrow_mut(self.mc).write_index(i, value)
        {
            return Ok(true);
        }
        // Numeric keys of typed arrays: the receiver is the array itself; write the element (out of bounds also counts
        // as success)
        if let (ValueKind::Object(o), ValueKind::Number(x)) = (target.kind(), key.kind())
            && matches!(o.borrow().kind, ObjectKind::TypedArray(..))
        {
            self.typed_set(o, x + 0.0, value)?;
            return Ok(true);
        }
        if target.is_nullish() {
            let k = self.to_string(key)?;
            return Err(self.nullish_error(target, "set", &k));
        }
        match self.to_property_key(key)? {
            PropertyKey::Symbol(k) => self.try_set_symbol(target, k, value),
            PropertyKey::String(k) => self.try_set_named(target, &k, value),
        }
    }

    /// `target[key] = value`: throws a TypeError if the write fails.
    pub fn set_keyed(&mut self, target: Value<'gc>, key: Value<'gc>, value: Value<'gc>) -> Result<(), Error> {
        if self.try_set_keyed(target, key, value)? {
            Ok(())
        } else {
            let k = match key.kind() {
                ValueKind::Symbol(s) => self.symbol_descriptive(s),
                k => self.to_string(k.into())?,
            };
            Err(self.read_only(&k))
        }
    }

    /// Assignment in JS code: a failed write throws a TypeError in strict mode and is silently dropped in sloppy mode.
    pub fn put_named(&mut self, target: Value<'gc>, key: &str, value: Value<'gc>, strict: bool) -> Result<(), Error> {
        if strict { self.set_named(target, key, value) } else { self.try_set_named(target, key, value).map(drop) }
    }

    pub fn put_keyed(&mut self, target: Value<'gc>, key: Value<'gc>, value: Value<'gc>, strict: bool) -> Result<(), Error> {
        if strict { self.set_keyed(target, key, value) } else { self.try_set_keyed(target, key, value).map(drop) }
    }

    /// Define an own property (object literals, classes, CreateDataPropertyOrThrow): throws a TypeError if the
    /// definition fails.
    pub fn define(&mut self, o: JsObject<'gc>, key: &str, prop: Property<'gc>) -> Result<(), Error> {
        self.define_key(o, Key::Str(key), prop)
    }

    pub fn define_key(&mut self, o: JsObject<'gc>, key: Key<'_, 'gc>, prop: Property<'gc>) -> Result<(), Error> {
        if self.define_own_property(o, key, prop.into())? {
            Ok(())
        } else {
            let k = self.key_text(key);
            Err(self.throw(ErrorKind::Type, &format!("Cannot define property {k}, object is not extensible")))
        }
    }

    /// Define half of an accessor (the other half is kept if present).
    pub fn define_accessor(&mut self, o: JsObject<'gc>, key: &str, f: JsObject<'gc>, setter: bool, enumerable: bool) {
        self.define_accessor_key(o, Key::Str(key), f, setter, enumerable);
    }

    fn define_accessor_key(&mut self, o: JsObject<'gc>, key: Key<'_, 'gc>, f: JsObject<'gc>, setter: bool, enumerable: bool) {
        let (mut get, mut set) = match o.borrow().own_slot(key).map(|p| p.slot) {
            Some(Slot::Accessor { get, set }) => (get, set),
            _ => (None, None),
        };
        if setter { set = Some(f) } else { get = Some(f) }
        o.borrow_mut(self.mc).store(key, Property { slot: Slot::Accessor { get, set }, enumerable, writable: true, configurable: true });
    }

    /// `key in target` (including the prototype chain and array indices).
    pub fn has_property(&mut self, target: Value<'gc>, key: &str) -> Result<bool, Error> {
        let Some(o) = target.as_object() else {
            let what = self.to_string(target)?;
            return Err(self.throw(ErrorKind::Type, &format!("Cannot use 'in' operator to search for '{key}' in {}", &**what)));
        };
        self.has_key(o, Key::Str(key))
    }

    /// `delete target.key`: non-configurable ones can't be deleted.
    pub fn delete(&mut self, target: Value<'gc>, key: &str) -> Result<bool, Error> {
        self.delete_key(target, Key::Str(key))
    }

    /// CanBeHeldWeakly: an object, or a Symbol not registered with `Symbol.for`.
    pub fn can_be_held_weakly(&self, v: Value<'gc>) -> bool {
        match v.kind() {
            ValueKind::Object(_) => true,
            ValueKind::Symbol(s) => !self.realm.symbol_registry.get().iter().any(|(_, x)| Gc::ptr_eq(*x, s)),
            _ => false,
        }
    }

    /// A value in error messages: calls no user code (ToString is observable, so error reporting can't use it).
    pub fn describe(&self, v: Value<'gc>) -> String {
        match v.kind() {
            ValueKind::Undefined | ValueKind::Uninit => String::from("undefined"),
            ValueKind::Null => String::from("null"),
            ValueKind::Bool(b) => String::from(if b { "true" } else { "false" }),
            ValueKind::Number(x) => String::from(&*number_text(x)),
            ValueKind::String(s) => format!("\"{}\"", &**s),
            ValueKind::Symbol(s) => String::from(&**self.symbol_descriptive(s)),
            ValueKind::BigInt(b) => format!("{}n", b.0),
            ValueKind::Object(o) => {
                let d = o.borrow();
                if d.kind.is_callable() {
                    match d.get_own("name").and_then(|p| p.value()).map(Value::kind) {
                        Some(ValueKind::String(n)) if !n.is_empty() => String::from(&**n),
                        _ => String::from("function"),
                    }
                } else if matches!(d.kind, ObjectKind::Array) {
                    String::from("[object Array]")
                } else {
                    String::from("#<Object>")
                }
            }
        }
    }

    /// `v instanceof f`: check `f[Symbol.hasInstance]` first, then OrdinaryHasInstance.
    pub fn instance_of(&mut self, v: Value<'gc>, f: Value<'gc>) -> Result<bool, Error> {
        if f.as_object().is_none() {
            let what = self.describe(f);
            return Err(self.throw(ErrorKind::Type, &format!("Right-hand side of 'instanceof' is not an object: {what}")));
        }
        let h = self.get_symbol(f, self.realm.symbols.has_instance, f)?;
        if !h.is_nullish() {
            let r = self.call(h, f, &[v])?;
            return Ok(self.to_boolean(r));
        }
        if !self.is_callable(f) {
            return Err(self.throw(ErrorKind::Type, "Right-hand side of 'instanceof' is not callable"));
        }
        self.ordinary_has_instance(f, v)
    }

    /// OrdinaryHasInstance: a bound function is replaced by its target, then search `v`'s prototype chain for
    /// `f.prototype`.
    pub fn ordinary_has_instance(&mut self, f: Value<'gc>, v: Value<'gc>) -> Result<bool, Error> {
        if !self.is_callable(f) {
            return Ok(false);
        }
        if let Some(o) = f.as_object()
            && let ObjectKind::Bound(b) = &o.borrow().kind
        {
            let target = b.target;
            return self.instance_of(v, target);
        }
        let Some(o) = v.as_object() else { return Ok(false) };
        let Some(proto) = (self.get_named(f, "prototype")?).as_object() else {
            return Err(self.throw(ErrorKind::Type, "Function has non-object prototype in instanceof check"));
        };
        let mut cur = self.get_prototype_of(o)?;
        while let Some(c) = cur {
            if Gc::ptr_eq(c, proto) {
                return Ok(true);
            }
            cur = self.get_prototype_of(c)?;
        }
        Ok(false)
    }

    /// ToIntegerOrInfinity.
    pub fn to_integer_or_infinity(&mut self, v: Value<'gc>) -> Result<f64, Error> {
        let x = self.to_number(v)?;
        /// Numbers with magnitude below this convert to i64 without overflow (converting back drops the fraction; -0
        /// also becomes +0)
        const I64_SAFE: f64 = 9_223_372_036_854_775_808.0;
        #[expect(clippy::cast_possible_truncation, clippy::cast_precision_loss, reason = "magnitude below 2^63; dropping the fraction is intended")]
        Ok(if x.is_nan() {
            0.0
        } else if x.abs() < I64_SAFE {
            (x as i64) as f64
        } else {
            libm::trunc(x) + 0.0
        })
    }

    /// ToLength: clamp to `[0, 2^53 − 1]`.
    pub fn to_length(&mut self, v: Value<'gc>) -> Result<f64, Error> {
        let n = self.to_integer_or_infinity(v)?;
        Ok(n.clamp(0.0, MAX_SAFE_INTEGER))
    }

    /// LengthOfArrayLike.
    pub fn length_of_array_like(&mut self, o: Value<'gc>) -> Result<f64, Error> {
        // A real array's `length` is the stored length (it can't be an accessor)
        if let Some(x) = o.as_object()
            && let Some(n) = x.borrow().array_length()
        {
            return Ok(f64::from(n));
        }
        let len = self.get_named(o, "length")?;
        self.to_length(len)
    }

    /// Number index ⇒ property key text.
    pub fn index_key(i: f64) -> String {
        String::from(&*number_text(i))
    }

    /// `o[i]` (fast path for the array dense part).
    pub fn get_index(&mut self, o: Value<'gc>, i: f64) -> Result<Value<'gc>, Error> {
        if let Some(x) = o.as_object()
            && matches!(x.borrow().kind, ObjectKind::TypedArray(..))
        {
            return Ok(self.typed_get(x, i).unwrap_or_default());
        }
        if let Some(x) = o.as_object()
            && x.is_array()
            && let Some(n) = index_of_number(i)
            && let Some(v) = x.elements().get(n as usize)
        {
            return Ok(v);
        }
        self.get_named(o, &Self::index_key(i))
    }

    /// `o[i] = v`; throws a TypeError if the write fails (Set(O, P, V, true)).
    pub fn set_index(&mut self, o: Value<'gc>, i: f64, v: Value<'gc>) -> Result<(), Error> {
        if let (ValueKind::Object(x), Some(n)) = (o.kind(), index_of_number(i))
            && x.borrow_mut(self.mc).write_index(n, v)
        {
            return Ok(());
        }
        self.set_named(o, &Self::index_key(i), v)
    }

    /// HasProperty(O, i).
    pub fn has_index(&mut self, o: JsObject<'gc>, i: f64) -> Result<bool, Error> {
        if o.is_array()
            && let Some(n) = index_of_number(i)
            && (n as usize) < o.elements().len()
        {
            return Ok(true);
        }
        self.has_key(o, Key::Str(&Self::index_key(i)))
    }

    /// If HasProperty(O, i) is true, Get(O, i), otherwise `None` (array methods accessing element by element). The
    /// dense part of a real array is read directly in one borrow.
    pub fn present_index(&mut self, o: JsObject<'gc>, i: f64) -> Result<Option<Value<'gc>>, Error> {
        if o.is_array()
            && let Some(n) = index_of_number(i)
            && let Some(v) = o.elements().get(n as usize)
        {
            return Ok(Some(v));
        }
        if self.has_index(o, i)? { self.get_index(o.into(), i).map(Some) } else { Ok(None) }
    }

    /// DeletePropertyOrThrow(O, i).
    pub fn delete_index(&mut self, o: JsObject<'gc>, i: f64) -> Result<(), Error> {
        let k = Self::index_key(i);
        if self.delete_property(o, Key::Str(&k))? {
            Ok(())
        } else {
            Err(self.throw(ErrorKind::Type, &format!("Cannot delete property '{k}' of {}", "[object Array]")))
        }
    }

    /// CreateDataPropertyOrThrow(O, i, v).
    pub fn create_index(&mut self, o: JsObject<'gc>, i: f64, v: Value<'gc>) -> Result<(), Error> {
        // Fast path: update a value in a real array's dense part, or append right at the dense end (extensible, length
        // writable, no sparse elements after it)
        if let Some(n) = index_of_number(i) {
            // The sparse part either ends before n or has no elements at all (never had a numeric key: just an extended
            // length, as in `new Array(len)`)
            let append = !o.non_extensible() && !o.length_readonly() && (o.sparse_length() <= n || !o.digit_keys());
            if o.is_array() {
                let (at, items) = (n as usize, o.elements());
                if at < items.len() {
                    items.set(self.mc, at, v);
                    return Ok(());
                }
                if at == items.len() && append {
                    items.push(self.mc, v);
                    return Ok(());
                }
            }
        }
        self.create_data_property_or_throw(o, Key::Str(&Self::index_key(i)), v)
    }

    /// IsArray.
    pub fn is_array(&mut self, v: Value<'gc>) -> Result<bool, Error> {
        let Some(o) = v.as_object() else { return Ok(false) };
        if let Some(parts) = self.proxy_parts(o) {
            let (t, _) = parts?;
            return self.is_array(t.into());
        }
        Ok(o.is_array())
    }

    /// CreateListFromArrayLike.
    pub fn list_from_array_like(&mut self, v: Value<'gc>) -> Result<Vec<Value<'gc>>, Error> {
        if v.as_object().is_none() {
            return Err(self.throw(ErrorKind::Type, "CreateListFromArrayLike called on non-object"));
        }
        let len = self.length_of_array_like(v)?;
        let mut out = Vec::new();
        let mut i = 0.0;
        while i < len {
            out.push(self.get_index(v, i)?);
            i += 1.0;
        }
        Ok(out)
    }

    /// GetIterator: call `v[Symbol.iterator]()` and take its `next`.
    pub fn get_iterator(&mut self, v: Value<'gc>) -> Result<IterRecord<'gc>, Error> {
        let method = if v.is_nullish() { Value::Undefined } else { self.get_symbol(v, self.realm.symbols.iterator, v)? };
        if !self.is_callable(method) {
            let what = match v.kind() {
                ValueKind::Symbol(s) => self.symbol_descriptive(s),
                _ => self.to_string(v)?,
            };
            return Err(self.throw(ErrorKind::Type, &format!("{} is not iterable", &**what)));
        }
        let iterator = self.call(method, v, &[])?;
        if iterator.as_object().is_none() {
            return Err(self.throw(ErrorKind::Type, "Result of the Symbol.iterator method is not an object"));
        }
        let next = self.get_named(iterator, "next")?;
        Ok(IterRecord { iterator, next })
    }

    /// Advance an array iterator one step (rereads `length` each time, so changes to the array during iteration are
    /// visible): returns `None` when done and marks it done.
    pub fn array_iter_step(&mut self, o: JsObject<'gc>) -> Result<Option<Value<'gc>>, Error> {
        let ObjectKind::Iterator(IteratorState::Array { target, index, kind }) = o.borrow().kind else {
            return Err(self.throw(ErrorKind::Type, "next method called on an incompatible receiver"));
        };
        let Some(target) = target else { return Ok(None) };
        // Take a dense array's length directly (same as reading `length`, with no observable steps)
        let len = match target.kind() {
            ValueKind::Object(t) => (t.is_array() && t.sparse_length() == 0).then(|| t.elements().len()),
            _ => None,
        };
        let len = match len {
            #[expect(clippy::cast_precision_loss, reason = "the element count is far below 2^53")]
            Some(n) => n as f64,
            None => {
                let l = self.get_named(target, "length")?;
                self.to_number(l)?
            }
        };
        let i = f64::from(u32::try_from(index).unwrap_or(u32::MAX));
        // A NaN len also counts as done
        let finished = i.partial_cmp(&len) != Some(core::cmp::Ordering::Less);
        if let ObjectKind::Iterator(IteratorState::Array { target, index, .. }) = &mut o.borrow_mut(self.mc).kind {
            if finished {
                *target = None;
            } else {
                *index += 1;
            }
        }
        if finished {
            return Ok(None);
        }
        Ok(Some(match kind {
            IterKind::Keys => i.into(),
            IterKind::Values => self.get_keyed(target, i.into())?,
            IterKind::Entries => {
                let v = self.get_keyed(target, i.into())?;
                self.array(alloc::vec![i.into(), v]).into()
            }
        }))
    }

    /// Advance a string iterator one step (one code point): returns `None` when done.
    pub fn string_iter_step(&mut self, o: JsObject<'gc>) -> Result<Option<Value<'gc>>, Error> {
        let ObjectKind::Iterator(IteratorState::String { s, pos }) = o.borrow().kind else {
            return Err(self.throw(ErrorKind::Type, "next method called on an incompatible receiver"));
        };
        let Some(c) = s[pos..].chars().next() else { return Ok(None) };
        if let ObjectKind::Iterator(IteratorState::String { pos, .. }) = &mut o.borrow_mut(self.mc).kind {
            *pos += c.len_utf8();
        }
        Ok(Some(if c.is_ascii() { self.unit_string(u16::from(c as u8)) } else { self.string(c.encode_utf8(&mut [0; 4])) }.into()))
    }

    /// Advance one step: returns `None` when done.
    pub fn iterator_step(&mut self, it: IterRecord<'gc>) -> Result<Option<Value<'gc>>, Error> {
        // `next` is the built-in array / string iterator `next`: the result object it builds can't be observed
        // otherwise, so step directly
        if let (ValueKind::Object(n), ValueKind::Object(o)) = (it.next.kind(), it.iterator.kind()) {
            let [array, string] = *self.realm.builtin_nexts.get();
            let (is_array, is_string) = match &o.borrow().kind {
                ObjectKind::Iterator(IteratorState::Array { .. }) => (true, false),
                ObjectKind::Iterator(IteratorState::String { .. }) => (false, true),
                _ => (false, false),
            };
            let builtin = if is_array { array } else if is_string { string } else { None };
            if builtin.is_some_and(|k| Gc::ptr_eq(k, n)) {
                return if is_array { self.array_iter_step(o) } else { self.string_iter_step(o) };
            }
        }
        let r = self.call(it.next, it.iterator, &[])?;
        if r.as_object().is_none() {
            let what = self.to_string(r)?;
            return Err(self.throw(ErrorKind::Type, &format!("Iterator result {} is not an object", &**what)));
        }
        let done = self.get_named(r, "done")?;
        if self.to_boolean(done) { Ok(None) } else { Ok(Some(self.get_named(r, "value")?)) }
    }

    /// Close an iterator early: call `return` if there is one.
    pub fn iterator_close(&mut self, iterator: Value<'gc>) -> Result<(), Error> {
        let ret = self.get_named(iterator, "return")?;
        if ret.is_nullish() {
            return Ok(());
        }
        let r = self.call(ret, iterator, &[])?;
        if r.as_object().is_none() {
            return Err(self.throw(ErrorKind::Type, "Iterator result is not an object"));
        }
        Ok(())
    }

    /// An iterator result object `{ value, done }`.
    pub fn iter_result(&self, value: Value<'gc>, done: bool) -> JsObject<'gc> {
        let o = self.object();
        let d = o.borrow_mut(self.mc);
        d.set("value", value);
        d.set("done", done.into());
        drop(d);
        o
    }

    /// A built-in iterator object (array / string).
    pub fn builtin_iterator(&self, state: IteratorState<'gc>) -> JsObject<'gc> {
        let proto = match state {
            IteratorState::Array { .. } => self.realm.array_iterator_proto,
            IteratorState::String { .. } => self.realm.string_iterator_proto,
        };
        ObjectData { kind: ObjectKind::Iterator(state), proto: Some(proto), ..ObjectData::default() }.alloc(self.mc)
    }

    /// An array iterator (`values` / `keys` / `entries`).
    pub fn array_iterator(&self, target: Value<'gc>, kind: IterKind) -> JsObject<'gc> {
        self.builtin_iterator(IteratorState::Array { target: Some(target), index: 0, kind })
    }

    /// Spread / rest elements: run the iterable to completion.
    pub fn to_list(&mut self, v: Value<'gc>) -> Result<Vec<Value<'gc>>, Error> {
        let it = self.get_iterator(v)?;
        let mut out = Vec::new();
        while let Some(x) = self.iterator_step(it)? {
            out.push(x);
        }
        Ok(out)
    }

    /// Object spread / rest (CopyDataProperties): define `src`'s own enumerable properties on `target`, skipping keys
    /// in `exclude`.
    pub fn copy_data(&mut self, target: JsObject<'gc>, src: Value<'gc>, exclude: &[JsString<'gc>]) -> Result<(), Error> {
        if src.is_nullish() {
            return Ok(());
        }
        let from = self.to_object(src)?;
        for k in self.own_property_keys(from)? {
            if let PropertyKey::String(s) = k
                && exclude.iter().any(|e| ***e == **s)
            {
                continue;
            }
            let key = k.key();
            if self.get_own_property(from, key)?.is_some_and(|p| p.enumerable) {
                let v = self.get_key(from.into(), key, from.into())?;
                self.create_data_property_or_throw(target, key, v)?;
            }
        }
        Ok(())
    }

    /// The keys `for-in` walks: own first, then along the prototype chain; enumerable string keys, deduplicated (those
    /// shadowed by non-enumerable properties don't appear either).
    pub fn for_in_keys(&mut self, v: Value<'gc>) -> Result<Vec<Value<'gc>>, Error> {
        let mut seen: Vec<JsString<'gc>> = Vec::new();
        let mut out = Vec::new();
        if v.is_nullish() {
            return Ok(out);
        }
        let mut cur = Some(self.to_object(v)?);
        while let Some(o) = cur {
            for k in self.own_property_keys(o)? {
                let PropertyKey::String(s) = k else { continue };
                if seen.iter().any(|x| **x == *s) {
                    continue;
                }
                seen.push(s);
                if self.get_own_property(o, Key::Str(&s))?.is_some_and(|p| p.enumerable) {
                    out.push(Value::String(s));
                }
            }
            cur = self.get_prototype_of(o)?;
        }
        Ok(out)
    }

    /// The `this` a construct call creates: prototype taken from `new_target.prototype`, or `Object.prototype` if that
    /// isn't an object.
    pub fn create_this(&mut self, new_target: Value<'gc>) -> Result<JsObject<'gc>, Error> {
        let proto = match (self.get_named(new_target, "prototype")?).kind() {
            ValueKind::Object(p) => p,
            _ => self.realm.object_proto,
        };
        Ok(ObjectData { proto: Some(proto), ..ObjectData::default() }.alloc(self.mc))
    }
}
