//! BigInt semantics: ToNumeric, parsing, arithmetic, comparison with Number.
use core::cmp::Ordering;

use r8gc::Gc;
use num_bigint::BigInt;
use num_traits::{FromPrimitive, One, Signed, ToPrimitive, Zero};

use crate::{Agent, BigIntData, BinaryOp, Error, ErrorKind, Hint, JsBigInt, Value};

/// Upper bound of the shift amount (in bits): any larger result wouldn't fit
const MAX_SHIFT: i64 = 1 << 30;

/// The result of ToNumeric.
#[derive(Clone, Copy, Debug)]
pub enum Numeric<'gc> {
    Number(f64),
    BigInt(JsBigInt<'gc>),
}

impl<'gc> Agent<'gc, '_> {
    /// A BigInt value on the heap.
    pub fn bigint(&self, b: BigInt) -> Value<'gc> {
        Value::BigInt(Gc::new(self.mc, BigIntData(b)))
    }

    /// ToNumeric: BigInts as is, everything else ToNumber.
    pub fn to_numeric(&mut self, v: Value<'gc>) -> Result<Numeric<'gc>, Error> {
        if let Value::Number(x) = v {
            return Ok(Numeric::Number(x));
        }
        match self.to_primitive(v, Hint::Number)? {
            Value::BigInt(b) => Ok(Numeric::BigInt(b)),
            p => Ok(Numeric::Number(self.to_number(p)?)),
        }
    }

    /// ToBigInt: numbers can't be converted directly (throws a TypeError); strings go through StringToBigInt (throws a
    /// SyntaxError if invalid).
    pub fn to_bigint(&mut self, v: Value<'gc>) -> Result<BigInt, Error> {
        match self.to_primitive(v, Hint::Number)? {
            Value::BigInt(b) => Ok(b.0.clone()),
            Value::Bool(t) => Ok(BigInt::from(u8::from(t))),
            Value::String(s) => Self::parse_bigint(&s).ok_or_else(|| self.throw(ErrorKind::Syntax, &alloc::format!("Cannot convert {} to a BigInt", &**s))),
            other => {
                let what = self.describe(other);
                Err(self.throw(ErrorKind::Type, &alloc::format!("Cannot convert {what} to a BigInt")))
            }
        }
    }

    /// A BigInt modulo 2^bits, mapped into the signed range when `signed` (BigInt.asIntN / asUintN, BigInt64Array
    /// elements).
    pub fn wrap_bigint(b: &BigInt, bits: u64, signed: bool) -> BigInt {
        if bits == 0 {
            return BigInt::zero();
        }
        let m = BigInt::one() << bits;
        let mut r = ((b % &m) + &m) % &m;
        if signed && r >= (BigInt::one() << (bits - 1)) {
            r -= m;
        }
        r
    }

    /// ToIndex: `undefined` is 0; an integer outside `[0, 2^53 − 1]` throws a RangeError.
    pub fn to_index(&mut self, v: Value<'gc>) -> Result<usize, Error> {
        if v == Value::Undefined {
            return Ok(0);
        }
        let n = self.to_integer_or_infinity(v)?;
        if !(0.0..=crate::ops::MAX_SAFE_INTEGER).contains(&n) {
            return Err(self.throw(ErrorKind::Range, "Invalid index"));
        }
        #[expect(clippy::cast_possible_truncation, clippy::cast_sign_loss, reason = "checked to be in [0, 2^53 − 1]")]
        Ok(n as usize)
    }

    /// `++` / `--` (`delta` is ±1).
    pub fn increment(&mut self, v: Value<'gc>, delta: i8) -> Result<Value<'gc>, Error> {
        if let Value::Number(x) = v {
            return Ok((x + f64::from(delta)).into());
        }
        Ok(match self.to_numeric(v)? {
            Numeric::Number(x) => (x + f64::from(delta)).into(),
            Numeric::BigInt(b) => self.bigint(&b.0 + BigInt::from(delta)),
        })
    }

    /// StringToBigInt: leading / trailing whitespace, `0x` / `0o` / `0b` prefixes, signed decimal; the empty string is
    /// 0.
    pub fn parse_bigint(s: &str) -> Option<BigInt> {
        let t = s.trim_matches(|c: char| (c.is_whitespace() && c != '\u{85}') || c == '\u{feff}');
        if t.is_empty() {
            return Some(BigInt::zero());
        }
        let radix = match t.get(..2) {
            Some("0x" | "0X") => 16,
            Some("0o" | "0O") => 8,
            Some("0b" | "0B") => 2,
            _ => 10,
        };
        if radix == 10 {
            let (neg, digits) = match t.as_bytes()[0] {
                b'-' => (true, &t[1..]),
                b'+' => (false, &t[1..]),
                _ => (false, t),
            };
            if digits.is_empty() || !digits.bytes().all(|c| c.is_ascii_digit()) {
                return None;
            }
            let n = BigInt::parse_bytes(digits.as_bytes(), 10)?;
            return Some(if neg { -n } else { n });
        }
        let digits = &t.as_bytes()[2..];
        // num-bigint accepts underscores; the spec doesn't
        if digits.is_empty() || !digits.iter().all(|&c| char::from(c).is_digit(radix)) {
            return None;
        }
        BigInt::parse_bytes(digits, radix)
    }

    /// An integral Number ⇒ BigInt (`None` if not an integer).
    pub fn number_to_bigint(x: f64) -> Option<BigInt> {
        (x.is_finite() && libm::trunc(x) == x).then(|| BigInt::from_f64(x)).flatten()
    }

    /// Compare a BigInt and a Number by mathematical value (NaN is incomparable).
    pub fn cmp_bigint_number(a: &BigInt, x: f64) -> Option<Ordering> {
        if x.is_nan() {
            return None;
        }
        if x.is_infinite() {
            return Some(if x > 0.0 { Ordering::Less } else { Ordering::Greater });
        }
        let t = libm::trunc(x);
        let b = BigInt::from_f64(t)?;
        Some(match a.cmp(&b) {
            Ordering::Equal if x > t => Ordering::Less,
            Ordering::Equal if x < t => Ordering::Greater,
            o => o,
        })
    }

    /// The numeric part of relational comparison (both sides already primitives, not both strings): `None` means
    /// incomparable.
    pub(crate) fn numeric_order(&mut self, l: Value<'gc>, r: Value<'gc>) -> Result<Option<Ordering>, Error> {
        Ok(match (l, r) {
            (Value::BigInt(a), Value::String(s)) => Self::parse_bigint(&s).map(|b| a.0.cmp(&b)),
            (Value::String(s), Value::BigInt(b)) => Self::parse_bigint(&s).map(|a| a.cmp(&b.0)),
            _ => match (self.to_numeric(l)?, self.to_numeric(r)?) {
                (Numeric::Number(a), Numeric::Number(b)) => a.partial_cmp(&b),
                (Numeric::BigInt(a), Numeric::BigInt(b)) => Some(a.0.cmp(&b.0)),
                (Numeric::BigInt(a), Numeric::Number(x)) => Self::cmp_bigint_number(&a.0, x),
                (Numeric::Number(x), Numeric::BigInt(b)) => Self::cmp_bigint_number(&b.0, x).map(Ordering::reverse),
            },
        })
    }

    /// Shift: shifts left when `left` is true, reversed for a negative amount; right shifts round toward −∞.
    fn shift(&self, a: &BigInt, by: &BigInt, left: bool) -> Result<BigInt, Error> {
        let n = by.to_i64().filter(|n| n.abs() <= MAX_SHIFT).ok_or_else(|| self.throw(ErrorKind::Range, "Maximum BigInt size exceeded"))?;
        let n = if left { n } else { -n };
        let amount = usize::try_from(n.unsigned_abs()).unwrap_or(usize::MAX);
        Ok(if n >= 0 { a << amount } else { a >> amount })
    }

    /// A binary operation on two BigInts.
    pub(crate) fn bigint_binary(&self, op: BinaryOp, a: &BigInt, b: &BigInt) -> Result<Value<'gc>, Error> {
        let r = match op {
            BinaryOp::Add => a + b,
            BinaryOp::Sub => a - b,
            BinaryOp::Mul => a * b,
            BinaryOp::Div | BinaryOp::Rem if b.is_zero() => return Err(self.throw(ErrorKind::Range, "Division by zero")),
            // num-bigint's / and % both round toward 0, matching the spec
            BinaryOp::Div => a / b,
            BinaryOp::Rem => a % b,
            BinaryOp::Exp => {
                if b.is_negative() {
                    return Err(self.throw(ErrorKind::Range, "Exponent must be non-negative"));
                }
                if a.is_zero() || a.abs().is_one() {
                    a.pow(b.to_u32().unwrap_or(if b.is_even() { 2 } else { 1 }))
                } else {
                    let e = b.to_u32().ok_or_else(|| self.throw(ErrorKind::Range, "Maximum BigInt size exceeded"))?;
                    a.pow(e)
                }
            }
            BinaryOp::BitAnd => a & b,
            BinaryOp::BitOr => a | b,
            BinaryOp::BitXor => a ^ b,
            BinaryOp::Shl => self.shift(a, b, true)?,
            BinaryOp::Shr => self.shift(a, b, false)?,
            BinaryOp::UShr => return Err(self.throw(ErrorKind::Type, "BigInts have no unsigned right shift, use >> instead")),
        };
        Ok(self.bigint(r))
    }
}

/// Evenness check (for the sign of `(±1)^e`).
trait Parity {
    fn is_even(&self) -> bool;
}

impl Parity for BigInt {
    fn is_even(&self) -> bool {
        (self % BigInt::from(2)).is_zero()
    }
}
