use j8framework::ValueKind;
use alloc::string::String;
use core::fmt::Write;
use alloc::vec::Vec;

use num_bigint::BigInt;
use num_traits::{One, Signed, ToPrimitive, Zero};
use j8framework::{Agent, Error, ErrorKind, JsSymbol, SymbolData, Value};
use unicode_normalization::UnicodeNormalization;

use crate::{Args, Installer};

/// The remaining String / Number / Math / Symbol / URI functions.
struct Texts;

/// Upper bound on fraction digits / significant digits
const MAX_DIGITS: f64 = 100.0;
/// Beyond this, toFixed falls back to ToString
const FIXED_LIMIT: f64 = 1e21;
/// Exponent of the smallest f64 subnormal (2^-1074)
const MIN_EXP: i64 = 1074;
/// Number of f64 mantissa bits (including the implicit bit)
const MANTISSA_BITS: u64 = 53;
/// Largest code point
const MAX_CODE_POINT: f64 = 1_114_111.0;
/// Reserved characters left unescaped in URIs
const URI_RESERVED: &str = ";/?:@&=+$,";
/// Unreserved punctuation left unescaped in URIs
const URI_MARK: &str = "-_.!~*'()";
/// Punctuation that `escape` leaves unescaped
const ESCAPE_KEEP: &str = "@*_+-./";

/// Normalization form.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Form {
    Nfc,
    Nfd,
    Nfkc,
    Nfkd,
}

impl Texts {
    // ---- String ----

    fn from_char_code<'gc>(a: &mut Agent<'gc, '_>, _: Value<'gc>, args: &[Value<'gc>]) -> Result<Value<'gc>, Error> {
        // A single non-surrogate code unit: encode it directly into a stack buffer (ToUint16 = low 16 bits of ToUint32)
        if let [v] = args {
            #[expect(clippy::cast_possible_truncation, reason = "ToUint16 just takes the low 16 bits")]
            let u = a.to_uint32(*v)? as u16;
            if let Some(c) = char::from_u32(u32::from(u)) {
                return Ok(a.string(c.encode_utf8(&mut [0; 4])).into());
            }
            return Ok(a.string(&String::from_utf16_lossy(&[u])).into());
        }
        let mut units = Vec::with_capacity(args.len());
        for &v in args {
            let n = a.to_number(v)?;
            let u = j8framework::ElementKind::Uint16.encode(&j8framework::Element::Number(n));
            units.push(u16::from_le_bytes([u[0], u[1]]));
        }
        Ok(a.string(&String::from_utf16_lossy(&units)).into())
    }

    fn from_code_point<'gc>(a: &mut Agent<'gc, '_>, _: Value<'gc>, args: &[Value<'gc>]) -> Result<Value<'gc>, Error> {
        let mut out = String::new();
        for &v in args {
            let n = a.to_number(v)?;
            if n != libm::trunc(n) || !(0.0..=MAX_CODE_POINT).contains(&n) {
                let t = a.describe(v);
                return Err(a.throw(ErrorKind::Range, &alloc::format!("Invalid code point {t}")));
            }
            #[expect(clippy::cast_possible_truncation, clippy::cast_sign_loss, reason = "checked to be within the code point range")]
            let cp = n as u32;
            out.push(char::from_u32(cp).unwrap_or('\u{fffd}'));
        }
        Ok(a.string(&out).into())
    }

    /// `String.raw(template, ...substitutions)`.
    fn raw<'gc>(a: &mut Agent<'gc, '_>, _: Value<'gc>, args: &[Value<'gc>]) -> Result<Value<'gc>, Error> {
        let cooked = a.to_object(args.at(0))?;
        let literals = a.get_named(cooked.into(), "raw")?;
        let literals = a.to_object(literals)?;
        let len = a.length_of_array_like(literals.into())?;
        let mut out = String::new();
        let mut i = 0.0;
        while i < len {
            let seg = a.get_index(literals.into(), i)?;
            out.push_str(&a.to_string(seg)?);
            if i + 1.0 >= len {
                break;
            }
            #[expect(clippy::cast_possible_truncation, clippy::cast_sign_loss, reason = "index is non-negative")]
            let k = i as usize + 1;
            if let Some(&sub) = args.get(k) {
                out.push_str(&a.to_string(sub)?);
            }
            i += 1.0;
        }
        Ok(a.string(&out).into())
    }

    fn this_string<'gc>(a: &mut Agent<'gc, '_>, this: Value<'gc>) -> Result<j8framework::JsString<'gc>, Error> {
        if this.is_nullish() {
            return Err(a.throw(ErrorKind::Type, "String.prototype method called on null or undefined"));
        }
        a.to_string(this)
    }

    fn code_point_at<'gc>(a: &mut Agent<'gc, '_>, this: Value<'gc>, args: &[Value<'gc>]) -> Result<Value<'gc>, Error> {
        let s = Self::this_string(a, this)?;
        let units: Vec<u16> = s.encode_utf16().collect();
        let pos = a.to_integer_or_infinity(args.at(0))?;
        #[expect(clippy::cast_precision_loss, reason = "string length is far below 2^53")]
        if pos < 0.0 || pos >= units.len() as f64 {
            return Ok(Value::Undefined);
        }
        #[expect(clippy::cast_possible_truncation, clippy::cast_sign_loss, reason = "checked to be in bounds")]
        let i = pos as usize;
        let first = units[i];
        let cp = match units.get(i + 1) {
            Some(&second) if (0xd800..0xdc00).contains(&first) && (0xdc00..0xe000).contains(&second) => 0x10000 + ((u32::from(first) - 0xd800) << 10) + (u32::from(second) - 0xdc00),
            _ => u32::from(first),
        };
        Ok(f64::from(cp).into())
    }

    fn is_well_formed<'gc>(a: &mut Agent<'gc, '_>, this: Value<'gc>, _: &[Value<'gc>]) -> Result<Value<'gc>, Error> {
        Self::this_string(a, this)?;
        Ok(true.into())
    }

    fn to_well_formed<'gc>(a: &mut Agent<'gc, '_>, this: Value<'gc>, _: &[Value<'gc>]) -> Result<Value<'gc>, Error> {
        Ok(Self::this_string(a, this)?.into())
    }

    fn locale_compare<'gc>(a: &mut Agent<'gc, '_>, this: Value<'gc>, args: &[Value<'gc>]) -> Result<Value<'gc>, Error> {
        let s = Self::this_string(a, this)?;
        let t = a.to_string(args.at(0))?;
        let (x, y): (String, String) = (s.nfc().collect(), t.nfc().collect());
        Ok(match x.cmp(&y) {
            core::cmp::Ordering::Less => -1.0,
            core::cmp::Ordering::Equal => 0.0,
            core::cmp::Ordering::Greater => 1.0,
        }
        .into())
    }

    fn normalize<'gc>(a: &mut Agent<'gc, '_>, this: Value<'gc>, args: &[Value<'gc>]) -> Result<Value<'gc>, Error> {
        let s = Self::this_string(a, this)?;
        let form = match args.at(0).kind() {
            ValueKind::Undefined => Form::Nfc,
            f => match &**a.to_string(Value::from(f))? {
                "NFC" => Form::Nfc,
                "NFD" => Form::Nfd,
                "NFKC" => Form::Nfkc,
                "NFKD" => Form::Nfkd,
                _ => return Err(a.throw(ErrorKind::Range, "The normalization form should be one of NFC, NFD, NFKC, NFKD.")),
            },
        };
        let out: String = match form {
            Form::Nfc => s.nfc().collect(),
            Form::Nfd => s.nfd().collect(),
            Form::Nfkc => s.nfkc().collect(),
            Form::Nfkd => s.nfkd().collect(),
        };
        Ok(a.string(&out).into())
    }

    /// `substr(start, length)` (annexB).
    fn substr<'gc>(a: &mut Agent<'gc, '_>, this: Value<'gc>, args: &[Value<'gc>]) -> Result<Value<'gc>, Error> {
        let s = Self::this_string(a, this)?;
        let units: Vec<u16> = s.encode_utf16().collect();
        #[expect(clippy::cast_precision_loss, reason = "string length is far below 2^53")]
        let size = units.len() as f64;
        let start = a.to_integer_or_infinity(args.at(0))?;
        let start = if start == f64::NEG_INFINITY { 0.0 } else if start < 0.0 { (size + start).max(0.0) } else { start.min(size) };
        let len = match args.at(1).kind() {
            ValueKind::Undefined => size,
            v => a.to_integer_or_infinity(Value::from(v))?,
        };
        let end = (start + len).clamp(start, size);
        #[expect(clippy::cast_possible_truncation, clippy::cast_sign_loss, reason = "clamped to [0, size]")]
        let (s0, e0) = (start as usize, end as usize);
        Ok(a.string(&String::from_utf16_lossy(&units[s0..e0])).into())
    }

    /// Annex B HTML methods: `<tag attr="value">s</tag>`.
    fn html<'gc>(a: &mut Agent<'gc, '_>, this: Value<'gc>, args: &[Value<'gc>], tag: &str, attr: Option<&str>) -> Result<Value<'gc>, Error> {
        let s = Self::this_string(a, this)?;
        let mut open = alloc::format!("<{tag}");
        if let Some(attr) = attr {
            let v = a.to_string(args.at(0))?;
            let _ = write!(open, " {attr}=\"{}\"", v.replace('"', "&quot;"));
        }
        Ok(a.string(&alloc::format!("{open}>{}</{tag}>", &**s)).into())
    }

    // ---- Number ----

    fn this_number<'gc>(a: &Agent<'gc, '_>, this: Value<'gc>, method: &str) -> Result<f64, Error> {
        match this.unwrapped().kind() {
            ValueKind::Number(x) => Ok(x),
            _ => Err(a.throw(ErrorKind::Type, &alloc::format!("Number.prototype.{method} requires that 'this' be a Number"))),
        }
    }

    /// round_half_up(x · 10^k), computed exactly; `x` is non-negative and finite.
    fn scaled(x: f64, k: i64) -> BigInt {
        let bits = x.to_bits();
        let exp = i64::try_from((bits >> 52) & 0x7ff).unwrap_or(0);
        let frac = bits & ((1 << 52) - 1);
        let (m, e) = if exp == 0 { (frac, 1 - 1075) } else { (frac | (1 << 52), exp - 1075) };
        let mut num = BigInt::from(m);
        let mut den = BigInt::one();
        let ten = BigInt::from(10);
        if k >= 0 {
            num *= num_traits::pow(ten, usize::try_from(k).unwrap_or(0));
        } else {
            den *= num_traits::pow(ten, usize::try_from(-k).unwrap_or(0));
        }
        if e >= 0 {
            num <<= usize::try_from(e).unwrap_or(0);
        } else {
            den <<= usize::try_from(-e).unwrap_or(0);
        }
        (num * 2 + &den) / (den * 2)
    }

    /// Decimal significand `n` (exactly `digits` digits) and exponent `e` such that n · 10^(e − digits + 1) is closest
    /// to x.
    fn significant(x: f64, digits: i64) -> (String, i64) {
        #[expect(clippy::cast_possible_truncation, reason = "exponent is within ±400")]
        let mut ei = libm::floor(libm::log10(x)) as i64;
        loop {
            let n = Self::scaled(x, digits - 1 - ei);
            let s = n.to_str_radix(10);
            let len = i64::try_from(s.len()).unwrap_or(0);
            match len.cmp(&digits) {
                core::cmp::Ordering::Greater => ei += 1,
                core::cmp::Ordering::Less => ei -= 1,
                core::cmp::Ordering::Equal => return (s, ei),
            }
        }
    }

    fn to_fixed<'gc>(a: &mut Agent<'gc, '_>, this: Value<'gc>, args: &[Value<'gc>]) -> Result<Value<'gc>, Error> {
        let x = Self::this_number(a, this, "toFixed")?;
        let f = a.to_integer_or_infinity(args.at(0))?;
        if !(0.0..=MAX_DIGITS).contains(&f) {
            return Err(a.throw(ErrorKind::Range, "toFixed() digits argument must be between 0 and 100"));
        }
        if !x.is_finite() {
            return Ok(a.to_string(x.into())?.into());
        }
        if x.abs() >= FIXED_LIMIT {
            return Ok(a.to_string(x.into())?.into());
        }
        #[expect(clippy::cast_possible_truncation, reason = "checked to be in [0, 100]")]
        let fd = f as i64;
        let n = Self::scaled(x.abs(), fd).to_str_radix(10);
        let fdu = usize::try_from(fd).unwrap_or(0);
        let padded = if n.len() <= fdu { alloc::format!("{}{n}", "0".repeat(fdu + 1 - n.len())) } else { n };
        let (int, frac) = padded.split_at(padded.len() - fdu);
        let body = if fdu == 0 { String::from(int) } else { alloc::format!("{int}.{frac}") };
        let sign = if x < 0.0 { "-" } else { "" };
        Ok(a.string(&alloc::format!("{sign}{body}")).into())
    }

    fn exponential(digits: &str, e: i64) -> String {
        let (first, rest) = digits.split_at(1);
        let mantissa = if rest.is_empty() { String::from(first) } else { alloc::format!("{first}.{rest}") };
        let sign = if e >= 0 { "+" } else { "-" };
        alloc::format!("{mantissa}e{sign}{}", e.abs())
    }

    fn to_exponential<'gc>(a: &mut Agent<'gc, '_>, this: Value<'gc>, args: &[Value<'gc>]) -> Result<Value<'gc>, Error> {
        let x = Self::this_number(a, this, "toExponential")?;
        let f = a.to_integer_or_infinity(args.at(0))?;
        if !x.is_finite() {
            return Ok(a.to_string(x.into())?.into());
        }
        if !(0.0..=MAX_DIGITS).contains(&f) {
            return Err(a.throw(ErrorKind::Range, "toExponential() argument must be between 0 and 100"));
        }
        let sign = if x < 0.0 { "-" } else { "" };
        let ax = x.abs();
        let s = if ax == 0.0 {
            #[expect(clippy::cast_possible_truncation, clippy::cast_sign_loss, reason = "checked to be in [0, 100]")]
            let fd = if args.at(0) == Value::Undefined { 0 } else { f as usize };
            Self::exponential(&"0".repeat(fd + 1), 0)
        } else if args.at(0) == Value::Undefined {
            // Shortest digit count that round-trips
            let shortest = String::from(ryu_js::Buffer::new().format(ax));
            let digits: String = shortest.split('e').next().unwrap_or("").chars().filter(char::is_ascii_digit).collect::<String>().trim_start_matches('0').trim_end_matches('0').into();
            let digits = if digits.is_empty() { String::from("0") } else { digits };
            let n = i64::try_from(digits.len()).unwrap_or(1);
            let (d, e) = Self::significant(ax, n);
            Self::exponential(&d, e)
        } else {
            #[expect(clippy::cast_possible_truncation, reason = "checked to be in [0, 100]")]
            let (d, e) = Self::significant(ax, f as i64 + 1);
            Self::exponential(&d, e)
        };
        Ok(a.string(&alloc::format!("{sign}{s}")).into())
    }

    fn to_precision<'gc>(a: &mut Agent<'gc, '_>, this: Value<'gc>, args: &[Value<'gc>]) -> Result<Value<'gc>, Error> {
        let x = Self::this_number(a, this, "toPrecision")?;
        if args.at(0) == Value::Undefined {
            return Ok(a.to_string(x.into())?.into());
        }
        let p = a.to_integer_or_infinity(args.at(0))?;
        if !x.is_finite() {
            return Ok(a.to_string(x.into())?.into());
        }
        if !(1.0..=MAX_DIGITS).contains(&p) {
            return Err(a.throw(ErrorKind::Range, "toPrecision() argument must be between 1 and 100"));
        }
        #[expect(clippy::cast_possible_truncation, reason = "checked to be in [1, 100]")]
        let p = p as i64;
        let sign = if x < 0.0 { "-" } else { "" };
        let ax = x.abs();
        let (digits, e) = if ax == 0.0 { ("0".repeat(usize::try_from(p).unwrap_or(1)), 0) } else { Self::significant(ax, p) };
        let s = if e < -6 || e >= p {
            Self::exponential(&digits, e)
        } else if e == p - 1 {
            digits
        } else if e >= 0 {
            let (int, frac) = digits.split_at(usize::try_from(e + 1).unwrap_or(0));
            alloc::format!("{int}.{frac}")
        } else {
            alloc::format!("0.{}{digits}", "0".repeat(usize::try_from(-(e + 1)).unwrap_or(0)))
        };
        Ok(a.string(&alloc::format!("{sign}{s}")).into())
    }

    fn to_locale_string<'gc>(a: &mut Agent<'gc, '_>, this: Value<'gc>, _: &[Value<'gc>]) -> Result<Value<'gc>, Error> {
        let x = Self::this_number(a, this, "toLocaleString")?;
        Ok(a.to_string(x.into())?.into())
    }

    fn is_finite<'gc>(_: &mut Agent<'gc, '_>, _: Value<'gc>, args: &[Value<'gc>]) -> Result<Value<'gc>, Error> {
        Ok(matches!(args.at(0).kind(), ValueKind::Number(x) if x.is_finite()).into())
    }

    // ---- Math ----

    fn unary<'gc>(a: &mut Agent<'gc, '_>, args: &[Value<'gc>], f: fn(f64) -> f64) -> Result<Value<'gc>, Error> {
        Ok(f(a.to_number(args.at(0))?).into())
    }

    fn fround<'gc>(a: &mut Agent<'gc, '_>, _: Value<'gc>, args: &[Value<'gc>]) -> Result<Value<'gc>, Error> {
        #[expect(clippy::cast_possible_truncation, reason = "fround is exactly rounding to f32")]
        Self::unary(a, args, |x| f64::from(x as f32))
    }

    fn f16round<'gc>(a: &mut Agent<'gc, '_>, _: Value<'gc>, args: &[Value<'gc>]) -> Result<Value<'gc>, Error> {
        Self::unary(a, args, |x| j8framework::f16_to_f64(j8framework::f64_to_f16(x)))
    }

    fn clz32<'gc>(a: &mut Agent<'gc, '_>, _: Value<'gc>, args: &[Value<'gc>]) -> Result<Value<'gc>, Error> {
        Ok(f64::from(a.to_uint32(args.at(0))?.leading_zeros()).into())
    }

    fn imul<'gc>(a: &mut Agent<'gc, '_>, _: Value<'gc>, args: &[Value<'gc>]) -> Result<Value<'gc>, Error> {
        let x = a.to_int32(args.at(0))?;
        let y = a.to_int32(args.at(1))?;
        Ok(f64::from(x.wrapping_mul(y)).into())
    }

    /// `Math.random`: xorshift64*, seeded from the wall clock on first use.
    fn random<'gc>(a: &mut Agent<'gc, '_>, _: Value<'gc>, _: &[Value<'gc>]) -> Result<Value<'gc>, Error> {
        let mut s = a.realm.random_state.load(core::sync::atomic::Ordering::Relaxed);
        if s == 0 {
            s = u64::try_from(a.platform.wall_clock().as_nanos() & u128::from(u64::MAX)).unwrap_or(1) | 1;
        }
        s ^= s >> 12;
        s ^= s << 25;
        s ^= s >> 27;
        a.realm.random_state.store(s, core::sync::atomic::Ordering::Relaxed);
        let r = s.wrapping_mul(0x2545_f491_4f6c_dd1d) >> 11;
        #[expect(clippy::cast_precision_loss, reason = "takes 53 bits")]
        let x = r as f64 / 9_007_199_254_740_992.0;
        Ok(x.into())
    }

    /// `Math.sumPrecise(items)`: exact sum, rounded once.
    fn sum_precise<'gc>(a: &mut Agent<'gc, '_>, _: Value<'gc>, args: &[Value<'gc>]) -> Result<Value<'gc>, Error> {
        let items = args.at(0);
        if items.is_nullish() {
            return Err(a.throw(ErrorKind::Type, "Math.sumPrecise requires an iterable"));
        }
        let it = a.get_iterator(items)?;
        let (mut sum, mut pos_inf, mut neg_inf, mut nan, mut all_neg_zero) = (BigInt::zero(), false, false, false, true);
        while let Some(v) = a.iterator_step(it)? {
            let Some(x) = v.as_number() else {
                let _ = a.iterator_close(it.iterator);
                return Err(a.throw(ErrorKind::Type, "Math.sumPrecise items must be numbers"));
            };
            if x.is_nan() {
                nan = true;
            } else if x == f64::INFINITY {
                pos_inf = true;
            } else if x == f64::NEG_INFINITY {
                neg_inf = true;
            } else if !(x == 0.0 && x.is_sign_negative()) {
                all_neg_zero = false;
                sum += Self::exact(x);
            }
        }
        if nan || (pos_inf && neg_inf) {
            return Ok(f64::NAN.into());
        }
        if pos_inf || neg_inf {
            return Ok(if pos_inf { f64::INFINITY } else { f64::NEG_INFINITY }.into());
        }
        if all_neg_zero {
            return Ok((-0.0).into());
        }
        Ok(Self::to_f64(&sum).into())
    }

    /// Finite f64 ⇒ integer (in units of 2^-1074).
    fn exact(x: f64) -> BigInt {
        let bits = x.to_bits();
        let exp = i64::try_from((bits >> 52) & 0x7ff).unwrap_or(0);
        let frac = bits & ((1 << 52) - 1);
        let (m, shift) = if exp == 0 { (frac, 0) } else { (frac | (1 << 52), exp - 1) };
        let v = BigInt::from(m) << usize::try_from(shift).unwrap_or(0);
        if x < 0.0 { -v } else { v }
    }

    /// Integer in units of 2^-1074 ⇒ nearest f64 (ties to even).
    fn to_f64(n: &BigInt) -> f64 {
        if n.is_zero() {
            return 0.0;
        }
        let neg = n.is_negative();
        let mag = n.abs();
        let bits = mag.bits();
        let (mantissa, shift) = if bits <= MANTISSA_BITS {
            (mag.to_u64().unwrap_or(0), 0)
        } else {
            let shift = bits - MANTISSA_BITS;
            let m = &mag >> usize::try_from(shift).unwrap_or(0);
            let rem = &mag - (&m << usize::try_from(shift).unwrap_or(0));
            let half = BigInt::one() << usize::try_from(shift - 1).unwrap_or(0);
            let mut m = m.to_u64().unwrap_or(0);
            if rem > half || (rem == half && m % 2 == 1) {
                m += 1;
            }
            (m, shift)
        };
        #[expect(clippy::cast_precision_loss, reason = "mantissa is at most 2^54")]
        let v = libm::ldexp(mantissa as f64, i32::try_from(i64::try_from(shift).unwrap_or(0) - MIN_EXP).unwrap_or(0));
        if neg { -v } else { v }
    }

    // ---- Symbol ----

    fn symbol_for<'gc>(a: &mut Agent<'gc, '_>, _: Value<'gc>, args: &[Value<'gc>]) -> Result<Value<'gc>, Error> {
        let key = a.to_string(args.at(0))?;
        let registry = &a.realm.realm().get().symbol_registry;
        if let Some(&(_, s)) = registry.get().iter().find(|(k, _)| ***k == **key) {
            return Ok(s.into());
        }
        let fresh: JsSymbol<'gc> = a.mc.alloc(SymbolData { description: Some((&**key).into()), private: false });
        // Another thread may have registered the key meanwhile: then its symbol wins
        let s = registry.update_with(a.mc, |entries| {
            let mut entries = entries.clone();
            let found = entries.iter().find(|(k, _)| ***k == **key).map(|&(_, s)| s);
            let s = found.unwrap_or(fresh);
            if found.is_none() {
                entries.push((key, fresh));
            }
            (entries, s)
        });
        Ok(s.into())
    }

    fn key_for<'gc>(a: &mut Agent<'gc, '_>, _: Value<'gc>, args: &[Value<'gc>]) -> Result<Value<'gc>, Error> {
        let ValueKind::Symbol(s) = args.at(0).kind() else {
            let t = a.describe(args.at(0));
            return Err(a.throw(ErrorKind::Type, &alloc::format!("{t} is not a symbol")));
        };
        Ok(a.realm.symbol_registry.get().iter().find(|(_, x)| j8framework::Gc::ptr_eq(*x, s)).map_or(Value::Undefined, |&(k, _)| k.into()))
    }

    fn symbol_to_primitive<'gc>(a: &mut Agent<'gc, '_>, this: Value<'gc>, _: &[Value<'gc>]) -> Result<Value<'gc>, Error> {
        match this.unwrapped().kind() {
            v @ ValueKind::Symbol(_) => Ok(Value::from(v)),
            _ => Err(a.throw(ErrorKind::Type, "Symbol.prototype[Symbol.toPrimitive] requires that 'this' be a Symbol")),
        }
    }

    // ---- BigInt ----

    fn as_n<'gc>(a: &mut Agent<'gc, '_>, args: &[Value<'gc>], signed: bool) -> Result<Value<'gc>, Error> {
        let bits = a.to_index(args.at(0))?;
        let b = a.to_bigint(args.at(1))?;
        Ok(a.bigint(Agent::wrap_bigint(&b, u64::try_from(bits).unwrap_or(u64::MAX), signed)))
    }

    fn as_int_n<'gc>(a: &mut Agent<'gc, '_>, _: Value<'gc>, args: &[Value<'gc>]) -> Result<Value<'gc>, Error> {
        Self::as_n(a, args, true)
    }

    fn as_uint_n<'gc>(a: &mut Agent<'gc, '_>, _: Value<'gc>, args: &[Value<'gc>]) -> Result<Value<'gc>, Error> {
        Self::as_n(a, args, false)
    }

    fn bigint_to_locale_string<'gc>(a: &mut Agent<'gc, '_>, this: Value<'gc>, _: &[Value<'gc>]) -> Result<Value<'gc>, Error> {
        match this.unwrapped().kind() {
            ValueKind::BigInt(b) => Ok(a.string(&b.0.to_str_radix(10)).into()),
            _ => Err(a.throw(ErrorKind::Type, "BigInt.prototype.toLocaleString requires that 'this' be a BigInt")),
        }
    }

    // ---- URI ----

    fn encode<'gc>(a: &mut Agent<'gc, '_>, v: Value<'gc>, keep: &str) -> Result<Value<'gc>, Error> {
        let s = a.to_string(v)?;
        let mut out = String::new();
        for c in s.chars() {
            if c.is_ascii_alphanumeric() || URI_MARK.contains(c) || keep.contains(c) {
                out.push(c);
            } else {
                for b in c.encode_utf8(&mut [0; 4]).bytes() {
                    let _ = write!(out, "%{b:02X}");
                }
            }
        }
        Ok(a.string(&out).into())
    }

    fn uri_error(a: &Agent<'_, '_>) -> Error {
        a.throw(ErrorKind::Uri, "URI malformed")
    }

    fn hex_byte(b: &[u8], at: usize) -> Option<u8> {
        let h = |c: u8| char::from(c).to_digit(16);
        if b.get(at) != Some(&b'%') {
            return None;
        }
        let (x, y) = (h(*b.get(at + 1)?)?, h(*b.get(at + 2)?)?);
        u8::try_from(x * 16 + y).ok()
    }

    fn decode<'gc>(a: &mut Agent<'gc, '_>, v: Value<'gc>, preserve: &str) -> Result<Value<'gc>, Error> {
        let s = a.to_string(v)?;
        let b = s.as_bytes();
        let mut out: Vec<u8> = Vec::new();
        let mut i = 0;
        while i < b.len() {
            if b[i] != b'%' {
                out.push(b[i]);
                i += 1;
                continue;
            }
            let first = Self::hex_byte(b, i).ok_or_else(|| Self::uri_error(a))?;
            if first < 0x80 {
                if preserve.contains(char::from(first)) {
                    out.extend_from_slice(&b[i..i + 3]);
                } else {
                    out.push(first);
                }
                i += 3;
                continue;
            }
            let n = match first {
                0xc2..=0xdf => 2,
                0xe0..=0xef => 3,
                0xf0..=0xf4 => 4,
                _ => return Err(Self::uri_error(a)),
            };
            let mut bytes = alloc::vec![first];
            for k in 1..n {
                let x = Self::hex_byte(b, i + 3 * k).ok_or_else(|| Self::uri_error(a))?;
                if x & 0xc0 != 0x80 {
                    return Err(Self::uri_error(a));
                }
                bytes.push(x);
            }
            let decoded = core::str::from_utf8(&bytes).map_err(|_| Self::uri_error(a))?;
            out.extend_from_slice(decoded.as_bytes());
            i += 3 * n;
        }
        let text = String::from_utf8(out).map_err(|_| Self::uri_error(a))?;
        Ok(a.string(&text).into())
    }

    fn encode_uri<'gc>(a: &mut Agent<'gc, '_>, _: Value<'gc>, args: &[Value<'gc>]) -> Result<Value<'gc>, Error> {
        Self::encode(a, args.at(0), "#;/?:@&=+$,")
    }

    fn encode_uri_component<'gc>(a: &mut Agent<'gc, '_>, _: Value<'gc>, args: &[Value<'gc>]) -> Result<Value<'gc>, Error> {
        Self::encode(a, args.at(0), "")
    }

    fn decode_uri<'gc>(a: &mut Agent<'gc, '_>, _: Value<'gc>, args: &[Value<'gc>]) -> Result<Value<'gc>, Error> {
        let keep = alloc::format!("{URI_RESERVED}#");
        Self::decode(a, args.at(0), &keep)
    }

    fn decode_uri_component<'gc>(a: &mut Agent<'gc, '_>, _: Value<'gc>, args: &[Value<'gc>]) -> Result<Value<'gc>, Error> {
        Self::decode(a, args.at(0), "")
    }

    /// `escape` (annexB).
    fn escape<'gc>(a: &mut Agent<'gc, '_>, _: Value<'gc>, args: &[Value<'gc>]) -> Result<Value<'gc>, Error> {
        let s = a.to_string(args.at(0))?;
        let mut out = String::new();
        for u in s.encode_utf16() {
            match char::from_u32(u32::from(u)) {
                Some(c) if c.is_ascii_alphanumeric() || ESCAPE_KEEP.contains(c) => out.push(c),
                _ if u < 0x100 => _ = write!(out, "%{u:02X}"),
                _ => _ = write!(out, "%u{u:04X}"),
            }
        }
        Ok(a.string(&out).into())
    }

    /// `unescape` (annexB).
    fn unescape<'gc>(a: &mut Agent<'gc, '_>, _: Value<'gc>, args: &[Value<'gc>]) -> Result<Value<'gc>, Error> {
        let s = a.to_string(args.at(0))?;
        let units: Vec<u16> = s.encode_utf16().collect();
        let hex = |slice: &[u16]| -> Option<u16> {
            let mut v: u16 = 0;
            for &c in slice {
                v = v * 16 + u16::try_from(char::from_u32(u32::from(c))?.to_digit(16)?).ok()?;
            }
            Some(v)
        };
        let mut out = Vec::new();
        let mut i = 0;
        while i < units.len() {
            let c = units[i];
            if c == u16::from(b'%') {
                if units.get(i + 1) == Some(&u16::from(b'u'))
                    && let Some(v) = units.get(i + 2..i + 6).and_then(hex)
                {
                    out.push(v);
                    i += 6;
                    continue;
                }
                if let Some(v) = units.get(i + 1..i + 3).and_then(hex) {
                    out.push(v);
                    i += 3;
                    continue;
                }
            }
            out.push(c);
            i += 1;
        }
        Ok(a.string(&String::from_utf16_lossy(&out)).into())
    }
}

/// A group of host functions: each name maps to a closure body (`$body` gets `a` / `this` / `args`).
macro_rules! natives {
    ($($name:ident($a:ident, $this:ident, $args:ident) $body:block)*) => {
        impl Texts {
            $(fn $name<'gc>($a: &mut Agent<'gc, '_>, $this: Value<'gc>, $args: &[Value<'gc>]) -> Result<Value<'gc>, Error> $body)*
        }
    };
}

natives! {
    sinh(a, _this, args) { Self::unary(a, args, libm::sinh) }
    cosh(a, _this, args) { Self::unary(a, args, libm::cosh) }
    tanh(a, _this, args) { Self::unary(a, args, libm::tanh) }
    asinh(a, _this, args) { Self::unary(a, args, libm::asinh) }
    acosh(a, _this, args) { Self::unary(a, args, libm::acosh) }
    atanh(a, _this, args) { Self::unary(a, args, libm::atanh) }
    anchor(a, this, args) { Self::html(a, this, args, "a", Some("name")) }
    big(a, this, args) { Self::html(a, this, args, "big", None) }
    blink(a, this, args) { Self::html(a, this, args, "blink", None) }
    bold(a, this, args) { Self::html(a, this, args, "b", None) }
    fixed(a, this, args) { Self::html(a, this, args, "tt", None) }
    fontcolor(a, this, args) { Self::html(a, this, args, "font", Some("color")) }
    fontsize(a, this, args) { Self::html(a, this, args, "font", Some("size")) }
    italics(a, this, args) { Self::html(a, this, args, "i", None) }
    link(a, this, args) { Self::html(a, this, args, "a", Some("href")) }
    small(a, this, args) { Self::html(a, this, args, "small", None) }
    strike(a, this, args) { Self::html(a, this, args, "strike", None) }
    sub(a, this, args) { Self::html(a, this, args, "sub", None) }
    sup(a, this, args) { Self::html(a, this, args, "sup", None) }
}

impl<'gc> Installer<'_, 'gc> {
    fn global_fn(&self, name: &str) -> Value<'gc> {
        self.realm.global.borrow().own(self.mc, name).unwrap_or_default()
    }

    pub(crate) fn texts(&mut self) {
        let r = self.realm;
        let string = self.global_fn("String");
        if let Some(ctor) = string.as_object() {
            self.method(ctor, "fromCharCode", Texts::from_char_code, 1);
            self.method(ctor, "fromCodePoint", Texts::from_code_point, 1);
            self.method(ctor, "raw", Texts::raw, 1);
        }
        let sp = r.string_proto;
        let methods: [(&str, j8framework::NativeFn, u32); 19] = [
            ("codePointAt", Texts::code_point_at, 1),
            ("isWellFormed", Texts::is_well_formed, 0),
            ("toWellFormed", Texts::to_well_formed, 0),
            ("localeCompare", Texts::locale_compare, 1),
            ("normalize", Texts::normalize, 0),
            ("substr", Texts::substr, 2),
            ("anchor", Texts::anchor, 1),
            ("big", Texts::big, 0),
            ("blink", Texts::blink, 0),
            ("bold", Texts::bold, 0),
            ("fixed", Texts::fixed, 0),
            ("fontcolor", Texts::fontcolor, 1),
            ("fontsize", Texts::fontsize, 1),
            ("italics", Texts::italics, 0),
            ("link", Texts::link, 1),
            ("small", Texts::small, 0),
            ("strike", Texts::strike, 0),
            ("sub", Texts::sub, 0),
            ("sup", Texts::sup, 0),
        ];
        for (name, f, n) in methods {
            self.method(sp, name, f, n);
        }
        // trimLeft / trimRight are the same functions as trimStart / trimEnd
        for (alias, original) in [("trimLeft", "trimStart"), ("trimRight", "trimEnd")] {
            let f = sp.borrow().own(self.mc, original).unwrap_or_default();
            self.set(sp, alias, f);
        }
        // Number
        if let Some(ctor) = self.global_fn("Number").as_object() {
            self.method(ctor, "isFinite", Texts::is_finite, 1);
            for name in ["parseFloat", "parseInt"] {
                let f = self.global_fn(name);
                self.set(ctor, name, f);
            }
        }
        let np = r.number_proto;
        self.method(np, "toFixed", Texts::to_fixed, 1);
        self.method(np, "toExponential", Texts::to_exponential, 1);
        self.method(np, "toPrecision", Texts::to_precision, 1);
        self.method(np, "toLocaleString", Texts::to_locale_string, 0);
        // Math
        if let Some(m) = self.global_fn("Math").as_object() {
            let math: [(&str, j8framework::NativeFn, u32); 12] = [
                ("sinh", Texts::sinh, 1),
                ("cosh", Texts::cosh, 1),
                ("tanh", Texts::tanh, 1),
                ("asinh", Texts::asinh, 1),
                ("acosh", Texts::acosh, 1),
                ("atanh", Texts::atanh, 1),
                ("fround", Texts::fround, 1),
                ("f16round", Texts::f16round, 1),
                ("clz32", Texts::clz32, 1),
                ("imul", Texts::imul, 2),
                ("random", Texts::random, 0),
                ("sumPrecise", Texts::sum_precise, 1),
            ];
            for (name, f, n) in math {
                self.method(m, name, f, n);
            }
        }
        // Symbol
        if let Some(ctor) = self.global_fn("Symbol").as_object() {
            self.method(ctor, "for", Texts::symbol_for, 1);
            self.method(ctor, "keyFor", Texts::key_for, 1);
        }
        let tp = r.named_native(self.mc, Texts::symbol_to_primitive, "[Symbol.toPrimitive]", 1);
        r.symbol_proto.borrow_mut(self.mc).define_symbol(r.symbols.to_primitive, j8framework::Property { slot: j8framework::Slot::Data(tp.into()), enumerable: false, writable: false, configurable: true });
        // BigInt
        if let Some(ctor) = self.global_fn("BigInt").as_object() {
            self.method(ctor, "asIntN", Texts::as_int_n, 2);
            self.method(ctor, "asUintN", Texts::as_uint_n, 2);
        }
        self.method(r.bigint_proto, "toLocaleString", Texts::bigint_to_locale_string, 0);
        // URI
        let g = r.global;
        self.method(g, "encodeURI", Texts::encode_uri, 1);
        self.method(g, "encodeURIComponent", Texts::encode_uri_component, 1);
        self.method(g, "decodeURI", Texts::decode_uri, 1);
        self.method(g, "decodeURIComponent", Texts::decode_uri_component, 1);
        self.method(g, "escape", Texts::escape, 1);
        self.method(g, "unescape", Texts::unescape, 1);
    }
}
