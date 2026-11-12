use alloc::string::String;
use alloc::vec::Vec;

use r8framework::{Agent, Error, ErrorKind, JsString, Value};

use crate::{Args, Installer};

/// 字符串最长这么多 UTF-16 码元（同 V8 的 2^29 − 24），再长抛 RangeError
const MAX_STRING_LENGTH: f64 = 536_870_888.0;

/// `String` / `Number` 与原型方法。下标都按 UTF-16 码元算。
pub(crate) struct Strings;

/// `Number.MAX_SAFE_INTEGER`
const MAX_SAFE_INTEGER: f64 = 9_007_199_254_740_991.0;

impl Strings {
    /// `this` 转字符串（`null` / `undefined` 上调抛 TypeError）。
    fn this<'gc>(a: &mut Agent<'gc, '_>, this: Value<'gc>) -> Result<JsString<'gc>, Error> {
        if this.is_nullish() {
            return Err(a.throw(ErrorKind::Type, "String.prototype method called on null or undefined"));
        }
        a.to_string(this)
    }

    fn units<'gc>(a: &mut Agent<'gc, '_>, this: Value<'gc>) -> Result<Vec<u16>, Error> {
        Ok(Self::this(a, this)?.encode_utf16().collect())
    }

    fn from_units<'gc>(a: &Agent<'gc, '_>, u: &[u16]) -> Value<'gc> {
        a.string(&String::from_utf16_lossy(u)).into()
    }

    fn count<'gc>(n: usize) -> Value<'gc> {
        Value::Number(f64::from(u32::try_from(n).unwrap_or(u32::MAX)))
    }

    /// 实参转成 `[0, len]` 里的整数下标（负数夹到 0）。
    fn clamp<'gc>(a: &mut Agent<'gc, '_>, v: Value<'gc>, len: usize, default: usize) -> Result<usize, Error> {
        if v == Value::Undefined {
            return Ok(default);
        }
        let x = libm::trunc(a.to_number(v)?);
        #[expect(clippy::cast_possible_truncation, clippy::cast_sign_loss, reason = "已夹到 [0, len]")]
        let r = if x.is_nan() || x <= 0.0 { 0 } else { (x.min(f64::from(u32::try_from(len).unwrap_or(u32::MAX)))) as usize };
        Ok(r)
    }

    /// 相对下标（负数从末尾数）。
    fn relative<'gc>(a: &mut Agent<'gc, '_>, v: Value<'gc>, len: usize, default: usize) -> Result<usize, Error> {
        if v == Value::Undefined {
            return Ok(default);
        }
        let x = libm::trunc(a.to_number(v)?);
        let len_f = f64::from(u32::try_from(len).unwrap_or(u32::MAX));
        let r = if x < 0.0 { (len_f + x).max(0.0) } else { x.min(len_f) };
        #[expect(clippy::cast_possible_truncation, clippy::cast_sign_loss, reason = "已夹到 [0, len]")]
        let r = if r.is_nan() { 0 } else { r as usize };
        Ok(r)
    }

    /// JS 的 WhiteSpace 与 LineTerminator（U+0085 不算，U+FEFF 算）。
    pub(crate) fn is_space(c: char) -> bool {
        (c.is_whitespace() && c != '\u{85}') || c == '\u{feff}'
    }

    pub(crate) fn find(hay: &[u16], needle: &[u16], from: usize) -> Option<usize> {
        (from..=hay.len().checked_sub(needle.len())?).find(|&i| hay[i..].starts_with(needle))
    }

    fn string<'gc>(a: &mut Agent<'gc, '_>, _: Value<'gc>, args: &[Value<'gc>]) -> Result<Value<'gc>, Error> {
        let s: Value<'gc> = match (args.first(), a.new_target) {
            (None, _) => a.string("").into(),
            (Some(&Value::Symbol(s)), Value::Undefined) => a.symbol_descriptive(s).into(),
            (Some(&v), _) => a.to_string(v)?.into(),
        };
        a.wrap_primitive(s, a.realm.string_proto)
    }

    /// thisStringValue（`toString` / `valueOf`）：字符串或 String 包装对象，别的抛 TypeError。
    fn to_string<'gc>(a: &mut Agent<'gc, '_>, this: Value<'gc>, _: &[Value<'gc>]) -> Result<Value<'gc>, Error> {
        match this.unwrapped() {
            v @ Value::String(_) => Ok(v),
            _ => Err(a.throw(ErrorKind::Type, "String.prototype.toString requires that 'this' be a String")),
        }
    }

    fn char_at<'gc>(a: &mut Agent<'gc, '_>, this: Value<'gc>, args: &[Value<'gc>]) -> Result<Value<'gc>, Error> {
        let u = Self::units(a, this)?;
        let i = Self::clamp(a, args.at(0), u.len(), 0)?;
        Ok(Self::from_units(a, u.get(i..=i).unwrap_or(&[])))
    }

    fn char_code_at<'gc>(a: &mut Agent<'gc, '_>, this: Value<'gc>, args: &[Value<'gc>]) -> Result<Value<'gc>, Error> {
        let u = Self::units(a, this)?;
        let i = Self::clamp(a, args.at(0), u.len(), 0)?;
        Ok(u.get(i).map_or(f64::NAN, |&c| f64::from(c)).into())
    }

    fn index_of<'gc>(a: &mut Agent<'gc, '_>, this: Value<'gc>, args: &[Value<'gc>]) -> Result<Value<'gc>, Error> {
        let s = Self::this(a, this)?;
        let n = a.to_string(args.at(0))?;
        // 全 ASCII：字节下标就是 UTF-16 码元下标，直接在 UTF-8 上找
        if s.is_ascii() {
            let from = Self::clamp(a, args.at(1), s.len(), 0)?;
            return Ok(s[from..].find(&**n).map_or(Value::Number(-1.0), |i| Self::count(from + i)));
        }
        let u: Vec<u16> = s.encode_utf16().collect();
        let n: Vec<u16> = n.encode_utf16().collect();
        let from = Self::clamp(a, args.at(1), u.len(), 0)?;
        Ok(Self::find(&u, &n, from).map_or(Value::Number(-1.0), Self::count))
    }

    fn last_index_of<'gc>(a: &mut Agent<'gc, '_>, this: Value<'gc>, args: &[Value<'gc>]) -> Result<Value<'gc>, Error> {
        let u = Self::units(a, this)?;
        let n: Vec<u16> = a.to_string(args.at(0))?.encode_utf16().collect();
        let last = u.len().checked_sub(n.len());
        Ok(last.and_then(|l| (0..=l).rev().find(|&i| u[i..].starts_with(&n))).map_or(Value::Number(-1.0), Self::count))
    }

    fn includes<'gc>(a: &mut Agent<'gc, '_>, this: Value<'gc>, args: &[Value<'gc>]) -> Result<Value<'gc>, Error> {
        let u = Self::units(a, this)?;
        let n: Vec<u16> = a.to_string(args.at(0))?.encode_utf16().collect();
        Ok(Self::find(&u, &n, 0).is_some().into())
    }

    fn starts_with<'gc>(a: &mut Agent<'gc, '_>, this: Value<'gc>, args: &[Value<'gc>]) -> Result<Value<'gc>, Error> {
        let u = Self::units(a, this)?;
        let n: Vec<u16> = a.to_string(args.at(0))?.encode_utf16().collect();
        let from = Self::clamp(a, args.at(1), u.len(), 0)?;
        Ok(u[from..].starts_with(&n).into())
    }

    fn ends_with<'gc>(a: &mut Agent<'gc, '_>, this: Value<'gc>, args: &[Value<'gc>]) -> Result<Value<'gc>, Error> {
        let u = Self::units(a, this)?;
        let n: Vec<u16> = a.to_string(args.at(0))?.encode_utf16().collect();
        let end = Self::clamp(a, args.at(1), u.len(), u.len())?;
        Ok(u[..end].ends_with(&n).into())
    }

    fn slice<'gc>(a: &mut Agent<'gc, '_>, this: Value<'gc>, args: &[Value<'gc>]) -> Result<Value<'gc>, Error> {
        let u = Self::units(a, this)?;
        let start = Self::relative(a, args.at(0), u.len(), 0)?;
        let end = Self::relative(a, args.at(1), u.len(), u.len())?;
        Ok(Self::from_units(a, u.get(start..end.max(start)).unwrap_or(&[])))
    }

    fn substring<'gc>(a: &mut Agent<'gc, '_>, this: Value<'gc>, args: &[Value<'gc>]) -> Result<Value<'gc>, Error> {
        let u = Self::units(a, this)?;
        let s = Self::clamp(a, args.at(0), u.len(), 0)?;
        let e = Self::clamp(a, args.at(1), u.len(), u.len())?;
        Ok(Self::from_units(a, &u[s.min(e)..s.max(e)]))
    }

    fn at<'gc>(a: &mut Agent<'gc, '_>, this: Value<'gc>, args: &[Value<'gc>]) -> Result<Value<'gc>, Error> {
        let u = Self::units(a, this)?;
        let x = libm::trunc(a.to_number(args.at(0))?);
        let len = f64::from(u32::try_from(u.len()).unwrap_or(u32::MAX));
        let i = if x < 0.0 { len + x } else { x };
        #[expect(clippy::cast_possible_truncation, clippy::cast_sign_loss, reason = "已检查在 [0, len) 里")]
        let v = if (0.0..len).contains(&i) { Self::from_units(a, &u[i as usize..=i as usize]) } else { Value::Undefined };
        Ok(v)
    }

    fn to_upper_case<'gc>(a: &mut Agent<'gc, '_>, this: Value<'gc>, _: &[Value<'gc>]) -> Result<Value<'gc>, Error> {
        let s = Self::this(a, this)?;
        Ok(a.string(&s.to_uppercase()).into())
    }

    fn to_lower_case<'gc>(a: &mut Agent<'gc, '_>, this: Value<'gc>, _: &[Value<'gc>]) -> Result<Value<'gc>, Error> {
        let s = Self::this(a, this)?;
        Ok(a.string(&s.to_lowercase()).into())
    }

    fn trim<'gc>(a: &mut Agent<'gc, '_>, this: Value<'gc>, _: &[Value<'gc>]) -> Result<Value<'gc>, Error> {
        let s = Self::this(a, this)?;
        Ok(a.string(s.trim_matches(Self::is_space)).into())
    }

    fn trim_start<'gc>(a: &mut Agent<'gc, '_>, this: Value<'gc>, _: &[Value<'gc>]) -> Result<Value<'gc>, Error> {
        let s = Self::this(a, this)?;
        Ok(a.string(s.trim_start_matches(Self::is_space)).into())
    }

    fn trim_end<'gc>(a: &mut Agent<'gc, '_>, this: Value<'gc>, _: &[Value<'gc>]) -> Result<Value<'gc>, Error> {
        let s = Self::this(a, this)?;
        Ok(a.string(s.trim_end_matches(Self::is_space)).into())
    }

    /// `split(sep, limit)`：`sep` 是字符串（正则留给阶段 5）；空串按码元拆。
    pub(crate) fn split<'gc>(a: &mut Agent<'gc, '_>, this: Value<'gc>, args: &[Value<'gc>]) -> Result<Value<'gc>, Error> {
        let s = Self::this(a, this)?;
        let limit = match args.at(1) {
            Value::Undefined => usize::MAX,
            v => a.to_uint32(v)? as usize,
        };
        let items: Vec<Value<'gc>> = match args.at(0) {
            Value::Undefined if limit == 0 => Vec::new(),
            Value::Undefined => alloc::vec![s.into()],
            sep => {
                let sep = a.to_string(sep)?;
                if sep.is_empty() {
                    // 空分隔符：按 UTF-16 码元一个一个拆（全 ASCII 时就是逐字节）
                    if s.is_ascii() {
                        s.as_bytes().iter().take(limit).map(|&c| a.string(core::str::from_utf8(&[c]).unwrap_or_default()).into()).collect()
                    } else {
                        s.encode_utf16().take(limit).map(|c| Self::from_units(a, &[c])).collect()
                    }
                } else {
                    // 串都是合法 UTF-8：在 UTF-8 上切和按 UTF-16 码元切结果一样，不用来回转
                    s.split(&**sep).take(limit).map(|p| a.string(p).into()).collect()
                }
            }
        };
        Ok(a.array(items).into())
    }

    fn repeat<'gc>(a: &mut Agent<'gc, '_>, this: Value<'gc>, args: &[Value<'gc>]) -> Result<Value<'gc>, Error> {
        let s = Self::this(a, this)?;
        let n = a.to_number(args.at(0))?;
        if n < 0.0 || n.is_infinite() {
            return Err(a.throw(ErrorKind::Range, "Invalid count value"));
        }
        let n = if n.is_nan() { 0.0 } else { libm::trunc(n) };
        if s.is_empty() || n == 0.0 {
            return Ok(a.string("").into());
        }
        #[expect(clippy::cast_precision_loss, reason = "长度比较，精度足够")]
        let total = s.encode_utf16().count() as f64 * n;
        if total > MAX_STRING_LENGTH {
            return Err(a.throw(ErrorKind::Range, "Invalid string length"));
        }
        #[expect(clippy::cast_possible_truncation, clippy::cast_sign_loss, reason = "已检查非负、乘积不超过字符串上限")]
        Ok(a.string(&s.repeat(n as usize)).into())
    }

    fn pad<'gc>(a: &mut Agent<'gc, '_>, this: Value<'gc>, args: &[Value<'gc>], start: bool) -> Result<Value<'gc>, Error> {
        let u = Self::units(a, this)?;
        // ToLength：负数与 NaN 当 0
        let target = libm::trunc(a.to_number(args.at(0))?);
        let fill: Vec<u16> = match args.at(1) {
            Value::Undefined => alloc::vec![u16::from(b' ')],
            v => a.to_string(v)?.encode_utf16().collect(),
        };
        #[expect(clippy::cast_precision_loss, reason = "长度比较，精度足够")]
        let len = u.len() as f64;
        if target.is_nan() || target <= len || fill.is_empty() {
            return Ok(Self::from_units(a, &u));
        }
        if target > MAX_STRING_LENGTH {
            return Err(a.throw(ErrorKind::Range, "Invalid string length"));
        }
        #[expect(clippy::cast_possible_truncation, clippy::cast_sign_loss, reason = "已检查在 (len, 字符串上限] 内")]
        let target = target as usize;
        let pad: Vec<u16> = fill.iter().copied().cycle().take(target - u.len()).collect();
        let out = if start { [pad, u].concat() } else { [u, pad].concat() };
        Ok(Self::from_units(a, &out))
    }

    fn pad_start<'gc>(a: &mut Agent<'gc, '_>, this: Value<'gc>, args: &[Value<'gc>]) -> Result<Value<'gc>, Error> {
        Self::pad(a, this, args, true)
    }

    fn pad_end<'gc>(a: &mut Agent<'gc, '_>, this: Value<'gc>, args: &[Value<'gc>]) -> Result<Value<'gc>, Error> {
        Self::pad(a, this, args, false)
    }

    fn concat<'gc>(a: &mut Agent<'gc, '_>, this: Value<'gc>, args: &[Value<'gc>]) -> Result<Value<'gc>, Error> {
        let mut out = String::from(&**Self::this(a, this)?);
        for &v in args {
            out.push_str(&a.to_string(v)?);
        }
        Ok(a.string(&out).into())
    }

    /// `Number(v)`：BigInt 换成最接近的数。
    fn number<'gc>(a: &mut Agent<'gc, '_>, _: Value<'gc>, args: &[Value<'gc>]) -> Result<Value<'gc>, Error> {
        let n = match args.first() {
            None => 0.0,
            Some(&v) => match a.to_numeric(v)? {
                r8framework::Numeric::Number(x) => x,
                r8framework::Numeric::BigInt(b) => num_traits::ToPrimitive::to_f64(&b.0).unwrap_or(f64::NAN),
            },
        };
        a.wrap_primitive(n.into(), a.realm.number_proto)
    }

    fn is_integer<'gc>(_: &mut Agent<'gc, '_>, _: Value<'gc>, args: &[Value<'gc>]) -> Result<Value<'gc>, Error> {
        Ok(matches!(args.at(0), Value::Number(x) if x.is_finite() && libm::trunc(x) == x).into())
    }

    fn is_safe_integer<'gc>(_: &mut Agent<'gc, '_>, _: Value<'gc>, args: &[Value<'gc>]) -> Result<Value<'gc>, Error> {
        Ok(matches!(args.at(0), Value::Number(x) if x.is_finite() && libm::trunc(x) == x && libm::fabs(x) <= MAX_SAFE_INTEGER).into())
    }

    fn number_is_nan<'gc>(_: &mut Agent<'gc, '_>, _: Value<'gc>, args: &[Value<'gc>]) -> Result<Value<'gc>, Error> {
        Ok(matches!(args.at(0), Value::Number(x) if x.is_nan()).into())
    }

    /// `n.toString(radix)`：整数部分逐位除，小数部分逐位乘（最多到双精度能区分的位数）。
    fn number_to_string<'gc>(a: &mut Agent<'gc, '_>, this: Value<'gc>, args: &[Value<'gc>]) -> Result<Value<'gc>, Error> {
        /// 小数部分最多出这么多位
        const MAX_FRACTION_DIGITS: usize = 52;
        const DIGITS: &[u8; 36] = b"0123456789abcdefghijklmnopqrstuvwxyz";
        let Value::Number(x) = this.unwrapped() else {
            return Err(a.throw(ErrorKind::Type, "Number.prototype.toString requires that 'this' be a Number"));
        };
        let radix = match args.at(0) {
            Value::Undefined => 10,
            r => a.to_int32(r)?,
        };
        if !(2..=36).contains(&radix) {
            return Err(a.throw(ErrorKind::Range, "toString() radix must be between 2 and 36"));
        }
        if radix == 10 || !x.is_finite() {
            return Ok(a.to_string(Value::Number(x))?.into());
        }
        let r = f64::from(radix);
        let (neg, x) = (x < 0.0, libm::fabs(x));
        let (mut int, mut frac) = (libm::trunc(x), x - libm::trunc(x));
        let mut digits = Vec::new();
        loop {
            let d = libm::fmod(int, r);
            #[expect(clippy::cast_possible_truncation, clippy::cast_sign_loss, reason = "余数在 [0, radix)")]
            digits.push(DIGITS[d as usize]);
            int = libm::trunc(int / r);
            if int == 0.0 {
                break;
            }
        }
        digits.reverse();
        if frac > 0.0 {
            digits.push(b'.');
            for _ in 0..MAX_FRACTION_DIGITS {
                frac *= r;
                let d = libm::trunc(frac);
                #[expect(clippy::cast_possible_truncation, clippy::cast_sign_loss, reason = "在 [0, radix)")]
                digits.push(DIGITS[d as usize]);
                frac -= d;
                if frac == 0.0 {
                    break;
                }
            }
        }
        let s = String::from_utf8(digits).unwrap_or_default();
        Ok(a.string(&if neg { alloc::format!("-{s}") } else { s }).into())
    }

    /// thisNumberValue（`Number.prototype.valueOf`）：数或 Number 包装对象，别的抛 TypeError。
    fn number_value_of<'gc>(a: &mut Agent<'gc, '_>, this: Value<'gc>, _: &[Value<'gc>]) -> Result<Value<'gc>, Error> {
        match this.unwrapped() {
            v @ Value::Number(_) => Ok(v),
            _ => Err(a.throw(ErrorKind::Type, "Number.prototype.valueOf requires that 'this' be a Number")),
        }
    }
}

impl Installer<'_, '_> {
    pub(crate) fn string(&mut self) {
        let proto = self.realm.string_proto;
        // String.prototype 本身是包着 "" 的 String 对象
        proto.borrow_mut(self.mc).kind = r8framework::ObjectKind::Primitive(self.text(""));
        self.constructor("String", Strings::string, proto, 1);
        let methods: [(&str, r8framework::NativeFn, u32); 25] = [
            ("toString", Strings::to_string, 0),
            ("valueOf", Strings::to_string, 0),
            ("charAt", Strings::char_at, 1),
            ("charCodeAt", Strings::char_code_at, 1),
            ("indexOf", Strings::index_of, 1),
            ("lastIndexOf", Strings::last_index_of, 1),
            ("includes", Strings::includes, 1),
            ("startsWith", Strings::starts_with, 1),
            ("endsWith", Strings::ends_with, 1),
            ("slice", Strings::slice, 2),
            ("substring", Strings::substring, 2),
            ("at", Strings::at, 1),
            ("toUpperCase", Strings::to_upper_case, 0),
            ("toLowerCase", Strings::to_lower_case, 0),
            ("trim", Strings::trim, 0),
            ("trimStart", Strings::trim_start, 0),
            ("trimEnd", Strings::trim_end, 0),
            ("split", Strings::split, 2),
            ("repeat", Strings::repeat, 1),
            ("padStart", Strings::pad_start, 1),
            ("padEnd", Strings::pad_end, 1),
            ("concat", Strings::concat, 1),
            ("toLocaleUpperCase", Strings::to_upper_case, 0),
            ("toLocaleLowerCase", Strings::to_lower_case, 0),
            ("trimLeft", Strings::trim_start, 0),
        ];
        for (name, f, n) in methods {
            self.method(proto, name, f, n);
        }
    }

    pub(crate) fn number(&mut self) {
        let proto = self.realm.number_proto;
        // Number.prototype 本身是包着 +0 的 Number 对象
        proto.borrow_mut(self.mc).kind = r8framework::ObjectKind::Primitive(0.0.into());
        let ctor = self.constructor("Number", Strings::number, proto, 1);
        self.method(ctor, "isInteger", Strings::is_integer, 1);
        self.method(ctor, "isSafeInteger", Strings::is_safe_integer, 1);
        self.method(ctor, "isNaN", Strings::number_is_nan, 1);
        self.set(ctor, "MAX_SAFE_INTEGER", MAX_SAFE_INTEGER.into());
        self.set(ctor, "MIN_SAFE_INTEGER", (-MAX_SAFE_INTEGER).into());
        self.set(ctor, "EPSILON", f64::EPSILON.into());
        self.set(ctor, "MAX_VALUE", f64::MAX.into());
        self.set(ctor, "MIN_VALUE", f64::from_bits(1).into());
        self.set(ctor, "POSITIVE_INFINITY", f64::INFINITY.into());
        self.set(ctor, "NEGATIVE_INFINITY", f64::NEG_INFINITY.into());
        self.set(ctor, "NaN", f64::NAN.into());
        self.method(proto, "toString", Strings::number_to_string, 1);
        self.method(proto, "valueOf", Strings::number_value_of, 0);
    }
}
