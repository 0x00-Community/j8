//! JS 规范里的运算语义（各执行层共用同一份）：类型转换、运算符、属性存取。
use alloc::format;
use alloc::string::String;
use alloc::vec::Vec;

use r8gc::Gc;

use num_traits::Zero;

use crate::{Agent, Descriptor, Error, Key, Numeric, IterKind, IteratorState, JsObject, JsString, JsSymbol, ObjectData, ObjectKind, Property, PropertyKey, Slot, Value};

/// 二元运算（结果是值）。
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum BinaryOp {
    Add,
    Sub,
    Mul,
    Div,
    Rem,
    Exp,
    BitAnd,
    BitOr,
    BitXor,
    Shl,
    Shr,
    UShr,
}

/// 比较（结果是布尔）。
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum CompareOp {
    Eq,
    Ne,
    StrictEq,
    StrictNe,
    Lt,
    Gt,
    Le,
    Ge,
}

/// 一元运算。
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum UnaryOp {
    Neg,
    Plus,
    Not,
    BitNot,
    Typeof,
    Void,
}

/// 内建错误种类（`Error` 本身在第一个）。
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

/// ToPrimitive 的偏好。
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Hint {
    Default,
    Number,
    String,
}

impl BinaryOp {
    pub const ALL: [BinaryOp; 12] = [BinaryOp::Add, BinaryOp::Sub, BinaryOp::Mul, BinaryOp::Div, BinaryOp::Rem, BinaryOp::Exp, BinaryOp::BitAnd, BinaryOp::BitOr, BinaryOp::BitXor, BinaryOp::Shl, BinaryOp::Shr, BinaryOp::UShr];

    pub fn name(self) -> &'static str {
        match self {
            BinaryOp::Add => "add",
            BinaryOp::Sub => "sub",
            BinaryOp::Mul => "mul",
            BinaryOp::Div => "div",
            BinaryOp::Rem => "rem",
            BinaryOp::Exp => "exp",
            BinaryOp::BitAnd => "bitand",
            BinaryOp::BitOr => "bitor",
            BinaryOp::BitXor => "bitxor",
            BinaryOp::Shl => "shl",
            BinaryOp::Shr => "shr",
            BinaryOp::UShr => "ushr",
        }
    }

    /// 两边都已是数时的结果（各执行层的快路，也是 [`Agent::binary`] 的最后一步）。
    #[inline]
    pub fn numbers(self, a: f64, b: f64) -> f64 {
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

impl CompareOp {
    pub const ALL: [CompareOp; 8] = [CompareOp::Eq, CompareOp::Ne, CompareOp::StrictEq, CompareOp::StrictNe, CompareOp::Lt, CompareOp::Gt, CompareOp::Le, CompareOp::Ge];

    pub fn name(self) -> &'static str {
        match self {
            CompareOp::Eq => "eq",
            CompareOp::Ne => "ne",
            CompareOp::StrictEq => "stricteq",
            CompareOp::StrictNe => "strictne",
            CompareOp::Lt => "lt",
            CompareOp::Gt => "gt",
            CompareOp::Le => "le",
            CompareOp::Ge => "ge",
        }
    }

    /// 两边都是数时的结果（`NaN` 与谁比都不成立，`==` 与 `===` 对数一样）。
    #[inline]
    pub fn numbers(self, a: f64, b: f64) -> bool {
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

impl UnaryOp {
    pub const ALL: [UnaryOp; 6] = [UnaryOp::Neg, UnaryOp::Plus, UnaryOp::Not, UnaryOp::BitNot, UnaryOp::Typeof, UnaryOp::Void];

    pub fn name(self) -> &'static str {
        match self {
            UnaryOp::Neg => "neg",
            UnaryOp::Plus => "plus",
            UnaryOp::Not => "not",
            UnaryOp::BitNot => "bitnot",
            UnaryOp::Typeof => "typeof",
            UnaryOp::Void => "void",
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

/// 一个迭代器记录：迭代器对象与它的 `next` 方法。
#[derive(Clone, Copy, Debug, r8gc::Collect)]
#[collect(no_drop)]
pub struct IterRecord<'gc> {
    pub iterator: Value<'gc>,
    pub next: Value<'gc>,
}

/// 2^32：ToInt32 / ToUint32 的模。
const TWO_POW_32: f64 = 4_294_967_296.0;

/// 数组下标上限（`length` 最大 2^32 − 1）。
const MAX_INDEX: f64 = 4_294_967_294.0;

/// 字符串 ⇒ 数（StringToNumber）：前后空白、`Infinity`、`0x` / `0o` / `0b` 前缀、十进制。
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

/// 数 ⇒ 32 位整数的模 2^32 余数（ToInt32 / ToUint32 共用）。
/// 2^53 − 1
pub const MAX_SAFE_INTEGER: f64 = 9_007_199_254_740_991.0;

fn modulo_u32(x: f64) -> u32 {
    // 快路：已经是 i32 / u32 范围内的整数（绝大多数位运算的操作数），直接截
    #[expect(clippy::cast_possible_truncation, reason = "下面比较确认没截掉东西")]
    let i = x as i32;
    if f64::from(i) == x {
        return i.cast_unsigned();
    }
    #[expect(clippy::cast_possible_truncation, clippy::cast_sign_loss, reason = "下面比较确认没截掉东西")]
    let u = x as u32;
    if f64::from(u) == x {
        return u;
    }
    if !x.is_finite() {
        return 0;
    }
    let m = libm::trunc(x) % TWO_POW_32;
    let m = if m < 0.0 { m + TWO_POW_32 } else { m };
    // m 在 [0, 2^32) 里且是整数
    #[expect(clippy::cast_possible_truncation, clippy::cast_sign_loss, reason = "m 已规约到 [0, 2^32)")]
    let u = m as u32;
    u
}

/// Number::toString(x)：安全整数直接按十进制写进栈上缓冲（短，成 [`SmallStr`](crate::SmallStr) 不分配），其余交给 ryu-js。
pub fn number_text(x: f64) -> crate::SmallStr {
    #[expect(clippy::cast_possible_truncation, reason = "下面比较确认是安全整数")]
    let i = x as i64;
    #[expect(clippy::cast_precision_loss, reason = "安全整数范围内精确")]
    if x.abs() <= MAX_SAFE_INTEGER && i as f64 == x {
        // 最长 "-9007199254740991"
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
fn to_int32(x: f64) -> i32 {
    modulo_u32(x).cast_signed()
}

/// `%`：两边是安全整数时用整数取余（结果为 0 时带被除数的符号），否则走浮点 `fmod`。
fn remainder(a: f64, b: f64) -> f64 {
    if a.abs() <= MAX_SAFE_INTEGER && b.abs() <= MAX_SAFE_INTEGER && b != 0.0 {
        #[expect(clippy::cast_possible_truncation, reason = "已检查在安全整数范围内，下面比较确认是整数")]
        let (x, y) = (a as i64, b as i64);
        #[expect(clippy::cast_precision_loss, reason = "安全整数范围内精确")]
        if x as f64 == a && y as f64 == b {
            #[expect(clippy::cast_precision_loss, reason = "余数绝对值小于除数，精确")]
            let r = (x % y) as f64;
            return if r == 0.0 { 0.0f64.copysign(a) } else { r };
        }
    }
    a % b
}

/// `**`：JS 与 C `pow` 不同的地方是 `±1 ** ±∞` 与 `x ** NaN` 都是 NaN。
fn pow(a: f64, b: f64) -> f64 {
    if b.is_nan() || ((a == 1.0 || a == -1.0) && b.is_infinite()) { f64::NAN } else { libm::pow(a, b) }
}

/// 规范下标：非负整数且小于 2^32 − 1。
fn index_of_number(x: f64) -> Option<u32> {
    #[expect(clippy::cast_possible_truncation, clippy::cast_sign_loss, reason = "已检查是 [0, 2^32 − 2] 内的整数")]
    let i = ((0.0..=MAX_INDEX).contains(&x) && libm::trunc(x) == x).then_some(x as u32);
    i
}

/// 规范下标的字符串形式（`"0"`、`"12"`，不带前导零）。
pub(crate) fn index_of_str(s: &str) -> Option<u32> {
    let canonical = s == "0" || (!s.starts_with('0') && !s.is_empty() && s.bytes().all(|c| c.is_ascii_digit()));
    canonical.then(|| s.parse().ok()).flatten().filter(|&i| f64::from(i) <= MAX_INDEX)
}

impl<'gc> Agent<'gc, '_> {
    /// 字符串放上堆。
    pub fn string(&self, s: &str) -> JsString<'gc> {
        Gc::new(self.mc, crate::SmallStr::from(s))
    }

    /// 以 `Object.prototype` 为原型的空对象。
    pub fn object(&self) -> JsObject<'gc> {
        ObjectData { proto: Some(self.realm.object_proto), ..ObjectData::default() }.alloc(self.mc)
    }

    /// 以 `Array.prototype` 为原型的数组。
    pub fn array(&self, items: Vec<Value<'gc>>) -> JsObject<'gc> {
        ObjectData { kind: ObjectKind::Array(items), proto: Some(self.realm.array_proto), ..ObjectData::default() }.alloc(self.mc)
    }

    /// 一个 Error 对象（原型按种类取，自有 `message`）。
    pub fn error(&self, kind: ErrorKind, message: &str) -> JsObject<'gc> {
        let e = ObjectData { kind: ObjectKind::Error, proto: Some(self.realm.error_proto(kind)), ..ObjectData::default() }.alloc(self.mc);
        let m = self.string(message);
        e.borrow_mut(self.mc).define("message", Property::hidden(m.into()));
        e
    }

    /// 抛出一个内建错误。
    pub fn throw(&self, kind: ErrorKind, message: &str) -> Error {
        Error::Exception(self.global(self.error(kind, message).into()))
    }

    /// 抛出任意值。
    pub fn throw_value(&self, v: Value<'gc>) -> Error {
        Error::Exception(self.global(v))
    }

    pub fn is_callable(&self, v: Value<'gc>) -> bool {
        matches!(v, Value::Object(o) if o.borrow().kind.is_callable())
    }

    #[inline]
    pub fn to_boolean(&self, v: Value<'gc>) -> bool {
        match v {
            Value::Undefined | Value::Null | Value::Uninit => false,
            Value::Bool(b) => b,
            Value::Number(x) => !(x == 0.0 || x.is_nan()),
            // 按字节数看，不拼平绳索
            Value::String(s) => s.byte_len() != 0,
            Value::BigInt(b) => !b.0.is_zero(),
            Value::Symbol(_) | Value::Object(_) => true,
        }
    }

    /// ToPrimitive：对象按偏好依次试 `valueOf` / `toString`，结果是原始值就用。
    pub fn to_primitive(&mut self, v: Value<'gc>, hint: Hint) -> Result<Value<'gc>, Error> {
        if !matches!(v, Value::Object(_)) {
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
            if matches!(r, Value::Object(_)) {
                return Err(self.throw(ErrorKind::Type, "Cannot convert object to primitive value"));
            }
            return Ok(r);
        }
        let order = if hint == Hint::String { ["toString", "valueOf"] } else { ["valueOf", "toString"] };
        for name in order {
            let f = self.get_named(v, name)?;
            if self.is_callable(f) {
                let r = self.call(f, v, &[])?;
                if !matches!(r, Value::Object(_)) {
                    return Ok(r);
                }
            }
        }
        Err(self.throw(ErrorKind::Type, "Cannot convert object to primitive value"))
    }

    /// ToNumber。
    pub fn to_number(&mut self, v: Value<'gc>) -> Result<f64, Error> {
        Ok(match v {
            Value::Undefined | Value::Uninit => f64::NAN,
            Value::Null => 0.0,
            Value::Bool(b) => f64::from(u8::from(b)),
            Value::Number(x) => x,
            Value::String(s) => parse_number(&s),
            Value::Symbol(_) => return Err(self.throw(ErrorKind::Type, "Cannot convert a Symbol value to a number")),
            Value::BigInt(_) => return Err(self.throw(ErrorKind::Type, "Cannot convert a BigInt value to a number")),
            Value::Object(_) => {
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

    /// ToString。
    pub fn to_string(&mut self, v: Value<'gc>) -> Result<JsString<'gc>, Error> {
        Ok(match v {
            Value::String(s) => s,
            Value::Undefined | Value::Uninit => self.string("undefined"),
            Value::Null => self.string("null"),
            Value::Bool(true) => self.string("true"),
            Value::Bool(false) => self.string("false"),
            Value::Number(x) => Gc::new(self.mc, number_text(x)),
            Value::Symbol(_) => return Err(self.throw(ErrorKind::Type, "Cannot convert a Symbol value to a string")),
            Value::BigInt(b) => self.string(&format!("{}", b.0)),
            Value::Object(_) => {
                let p = self.to_primitive(v, Hint::String)?;
                return self.to_string(p);
            }
        })
    }

    /// `String(sym)` / 报错信息里的 Symbol：`Symbol(描述)`。
    pub fn symbol_descriptive(&self, s: JsSymbol<'gc>) -> JsString<'gc> {
        self.string(&format!("Symbol({})", s.description.as_deref().unwrap_or("")))
    }

    pub fn type_of(&self, v: Value<'gc>) -> &'static str {
        match v {
            Value::Undefined | Value::Uninit => "undefined",
            Value::Bool(_) => "boolean",
            Value::Number(_) => "number",
            Value::String(_) => "string",
            Value::Symbol(_) => "symbol",
            Value::BigInt(_) => "bigint",
            Value::Object(o) if o.borrow().kind.is_callable() => "function",
            Value::Null | Value::Object(_) => "object",
        }
    }

    pub fn binary(&mut self, op: BinaryOp, lhs: Value<'gc>, rhs: Value<'gc>) -> Result<Value<'gc>, Error> {
        if let (Value::Number(a), Value::Number(b)) = (lhs, rhs) {
            return Ok(op.numbers(a, b).into());
        }
        // 加法：两边先 ToPrimitive 看有没有字符串；其余运算左边整个 ToNumeric 完再轮到右边（求值顺序看得见）
        let (l, r) = if op == BinaryOp::Add {
            let (l, r) = (self.to_primitive(lhs, Hint::Default)?, self.to_primitive(rhs, Hint::Default)?);
            if matches!(l, Value::String(_)) || matches!(r, Value::String(_)) {
                let (a, b) = (self.to_string(l)?, self.to_string(r)?);
                return Ok(Value::String(Gc::new(self.mc, crate::SmallStr::concat(&a, &b))));
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

    /// 宽松相等（`==`）。
    pub fn loose_equals(&mut self, a: Value<'gc>, b: Value<'gc>) -> Result<bool, Error> {
        Ok(match (a, b) {
            _ if a.is_nullish() && b.is_nullish() => true,
            (Value::Undefined | Value::Null, _) | (_, Value::Undefined | Value::Null) => false,
            (Value::Object(_), Value::Object(_)) | (Value::Symbol(_), Value::Symbol(_)) => a == b,
            (Value::Object(_), _) => {
                let p = self.to_primitive(a, Hint::Default)?;
                return self.loose_equals(p, b);
            }
            (_, Value::Object(_)) => {
                let p = self.to_primitive(b, Hint::Default)?;
                return self.loose_equals(a, p);
            }
            (Value::Symbol(_), _) | (_, Value::Symbol(_)) => false,
            (Value::BigInt(x), Value::BigInt(y)) => x.0 == y.0,
            (Value::BigInt(x), Value::String(s)) | (Value::String(s), Value::BigInt(x)) => Self::parse_bigint(&s).is_some_and(|y| x.0 == y),
            (Value::BigInt(x), _) | (_, Value::BigInt(x)) => {
                let other = if matches!(a, Value::BigInt(_)) { b } else { a };
                let n = self.to_number(other)?;
                Self::cmp_bigint_number(&x.0, n) == Some(core::cmp::Ordering::Equal)
            }
            (Value::Number(_), Value::String(_)) | (Value::String(_), Value::Number(_)) | (Value::Bool(_), _) | (_, Value::Bool(_)) => self.to_number(a)? == self.to_number(b)?,
            _ => a == b,
        })
    }

    pub fn compare(&mut self, op: CompareOp, lhs: Value<'gc>, rhs: Value<'gc>) -> Result<bool, Error> {
        if let (Value::Number(a), Value::Number(b)) = (lhs, rhs) {
            return Ok(op.numbers(a, b));
        }
        Ok(match op {
            CompareOp::Eq => self.loose_equals(lhs, rhs)?,
            CompareOp::Ne => !self.loose_equals(lhs, rhs)?,
            CompareOp::StrictEq => lhs == rhs,
            CompareOp::StrictNe => lhs != rhs,
            CompareOp::Lt | CompareOp::Gt | CompareOp::Le | CompareOp::Ge => {
                let (l, r) = (self.to_primitive(lhs, Hint::Number)?, self.to_primitive(rhs, Hint::Number)?);
                if let (Value::String(a), Value::String(b)) = (l, r) {
                    // 按 UTF-16 码元比
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

    /// ToObject：对象原样，原始值包成包装对象（原型是它那一类的原型），`undefined` / `null` 抛 TypeError。
    pub fn to_object(&self, v: Value<'gc>) -> Result<JsObject<'gc>, Error> {
        match (v, self.proto_of(v)) {
            (Value::Object(o), _) => Ok(o),
            (_, Some(proto)) => Ok(ObjectData { kind: ObjectKind::Primitive(v), proto: Some(proto), ..ObjectData::default() }.alloc(self.mc)),
            (_, None) => Err(self.throw(ErrorKind::Type, "Cannot convert undefined or null to object")),
        }
    }

    /// 原始值读属性时用的原型。
    fn proto_of(&self, v: Value<'gc>) -> Option<JsObject<'gc>> {
        match v {
            Value::String(_) => Some(self.realm.string_proto),
            Value::Number(_) => Some(self.realm.number_proto),
            Value::Bool(_) => Some(self.realm.boolean_proto),
            Value::Symbol(_) => Some(self.realm.symbol_proto),
            Value::BigInt(_) => Some(self.realm.bigint_proto),
            Value::Object(o) => Some(o),
            Value::Undefined | Value::Null | Value::Uninit => None,
        }
    }

    fn nullish_error(&self, target: Value<'gc>, verb: &str, key: &str) -> Error {
        let what = if target == Value::Undefined { "undefined" } else { "null" };
        self.throw(ErrorKind::Type, &format!("Cannot {verb} properties of {what} (reading '{key}')"))
    }

    /// [[Get]]：`holder` 上（沿原型链）读 `key`，getter 的 `this` 是 `receiver`。
    pub fn get_key(&mut self, holder: Value<'gc>, key: Key<'_, 'gc>, receiver: Value<'gc>) -> Result<Value<'gc>, Error> {
        if holder.is_nullish() {
            let k = self.key_text(key);
            return Err(self.nullish_error(holder, "read", &k));
        }
        // 字符串的下标与 `length` 不必包一层
        if let (Value::String(s), Key::Str(k)) = (holder, key) {
            if k == "length" {
                return Ok(f64::from(u32::try_from(s.encode_utf16().count()).unwrap_or(u32::MAX)).into());
            }
            if let Some(i) = index_of_str(k)
                && let Some(u) = s.encode_utf16().nth(i as usize)
            {
                return Ok(self.string(&String::from_utf16_lossy(&[u])).into());
            }
        }
        match self.proto_of(holder) {
            Some(o) => self.object_get(o, key, receiver),
            None => Ok(Value::Undefined),
        }
    }

    /// 对象的 [[Get]]：沿原型链找（碰到 Proxy 交给它）。
    pub fn object_get(&mut self, o: JsObject<'gc>, key: Key<'_, 'gc>, receiver: Value<'gc>) -> Result<Value<'gc>, Error> {
        let mut cur = o;
        loop {
            // 快路：普通对象一次借用查完自有属性，没有就直接上原型
            let fast = {
                let d = cur.borrow();
                d.kind.has_ordinary_properties().then(|| (d.own_slot(key), d.proto))
            };
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

    /// `holder` 上（沿原型链）读 `key`，getter 的 `this` 是 `receiver`。
    pub fn get(&mut self, holder: Value<'gc>, key: &str, receiver: Value<'gc>) -> Result<Value<'gc>, Error> {
        self.get_key(holder, Key::Str(key), receiver)
    }

    /// `target.key`。
    pub fn get_named(&mut self, target: Value<'gc>, key: &str) -> Result<Value<'gc>, Error> {
        self.get_key(target, Key::Str(key), target)
    }

    /// 属性键：ToString（数组下标的数快路在调用处）。
    pub fn property_key(&mut self, key: Value<'gc>) -> Result<JsString<'gc>, Error> {
        self.to_string(key)
    }

    /// `holder[sym]`，getter 的 `this` 是 `receiver`。
    pub fn get_symbol(&mut self, holder: Value<'gc>, key: JsSymbol<'gc>, receiver: Value<'gc>) -> Result<Value<'gc>, Error> {
        self.get_key(holder, Key::Sym(key), receiver)
    }

    /// [[Set]]（OrdinarySet）：沿原型链找到的 setter 就调它，不可写的数据属性不写，否则在 `receiver` 上改 / 建。
    /// 给出写成没有。
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

    /// 对象的 [[Set]]（OrdinarySet）：沿原型链找到的 setter 就调它，不可写的数据属性不写，否则在 `receiver` 上改 / 建；
    /// 碰到 Proxy 交给它。
    pub fn object_set(&mut self, o: JsObject<'gc>, key: Key<'_, 'gc>, value: Value<'gc>, receiver: Value<'gc>) -> Result<bool, Error> {
        let mut cur = o;
        let found = loop {
            // 快路：普通对象一次借用查完自有属性，没有就直接上原型
            let fast = {
                let d = cur.borrow();
                d.kind.has_ordinary_properties().then(|| (d.own_slot(key), d.proto))
            };
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
                if matches!(receiver, Value::Object(r) if Gc::ptr_eq(r, cur)) {
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
        let Value::Object(r) = receiver else { return Ok(false) };
        if let Some(done) = r.borrow_mut(self.mc).ordinary_put(key, value) {
            return Ok(done);
        }
        match self.get_own_property(r, key)? {
            Some(Property { slot: Slot::Accessor { .. }, .. } | Property { writable: false, .. }) => Ok(false),
            Some(_) => self.define_own_property(r, key, Descriptor::value(value)),
            None => self.create_data_property(r, key, value),
        }
    }

    /// `target[sym] = value`，给出写成没有。
    pub fn try_set_symbol(&mut self, target: Value<'gc>, key: JsSymbol<'gc>, value: Value<'gc>) -> Result<bool, Error> {
        self.set_key(target, Key::Sym(key), value, target)
    }

    /// `target[sym] = value`：写不成抛 TypeError（规范里内建函数的 Set(…, true)）。
    pub fn set_symbol(&mut self, target: Value<'gc>, key: JsSymbol<'gc>, value: Value<'gc>) -> Result<(), Error> {
        if self.try_set_symbol(target, key, value)? {
            Ok(())
        } else {
            let what = self.symbol_descriptive(key);
            Err(self.read_only(&what))
        }
    }

    /// 写不成的 TypeError：不可扩展的对象上加新属性，或者写只读属性。
    fn set_failure(&self, target: Value<'gc>, key: &str) -> Error {
        if let Value::Object(o) = target {
            let d = o.borrow();
            if d.non_extensible && d.own_slot(Key::Str(key)).is_none() {
                return self.throw(ErrorKind::Type, &format!("Cannot add property {key}, object is not extensible"));
            }
        }
        self.read_only(key)
    }

    fn read_only(&self, key: &str) -> Error {
        self.throw(ErrorKind::Type, &format!("Cannot assign to read only property '{key}'"))
    }

    /// `key in target`，键是任意值。
    pub fn has_keyed(&mut self, target: Value<'gc>, key: Value<'gc>) -> Result<bool, Error> {
        let Value::Object(o) = target else {
            let what = match key {
                Value::Symbol(_) => String::from("a symbol"),
                k => format!("'{}'", &**self.to_string(k)?),
            };
            let t = self.to_string(target)?;
            return Err(self.throw(ErrorKind::Type, &format!("Cannot use 'in' operator to search for {what} in {}", &**t)));
        };
        match self.to_property_key(key)? {
            PropertyKey::Symbol(k) => self.has_key(o, Key::Sym(k)),
            PropertyKey::String(k) => self.has_key(o, Key::Str(&k)),
        }
    }

    /// [[HasProperty]]：自有或原型链上有。
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

    /// `delete target[key]`，键是任意值。
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

    /// `delete target.key`：原始值先包成对象（字符串的下标与 `length` 删不掉）。
    pub fn delete_key(&mut self, target: Value<'gc>, key: Key<'_, 'gc>) -> Result<bool, Error> {
        if target.is_nullish() {
            let k = self.key_text(key);
            return Err(self.nullish_error(target, "delete", &k));
        }
        let o = self.to_object(target)?;
        self.delete_property(o, key)
    }

    /// 定义属性，键是任意值（计算键）。
    pub fn define_keyed(&mut self, o: JsObject<'gc>, key: Value<'gc>, prop: Property<'gc>) -> Result<(), Error> {
        match self.to_property_key(key)? {
            PropertyKey::Symbol(k) => self.define_key(o, Key::Sym(k), prop),
            PropertyKey::String(k) => self.define_key(o, Key::Str(&k), prop),
        }
    }

    /// 定义访问器的一半，键是任意值。
    pub fn define_accessor_keyed(&mut self, o: JsObject<'gc>, key: Value<'gc>, f: JsObject<'gc>, setter: bool, enumerable: bool) -> Result<(), Error> {
        match self.to_property_key(key)? {
            PropertyKey::Symbol(k) => self.define_accessor_key(o, Key::Sym(k), f, setter, enumerable),
            PropertyKey::String(k) => self.define_accessor_key(o, Key::Str(&k), f, setter, enumerable),
        }
        Ok(())
    }

    /// `target[key]`。
    pub fn get_keyed(&mut self, target: Value<'gc>, key: Value<'gc>) -> Result<Value<'gc>, Error> {
        if let Value::Symbol(k) = key {
            return self.get_symbol(target, k, target);
        }
        if let (Value::Object(o), Value::Number(x)) = (target, key)
            && let (ObjectKind::Array(items), Some(i)) = (&o.borrow().kind, index_of_number(x))
            && let Some(&v) = items.get(i as usize)
        {
            return Ok(v);
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

    /// `target.key = value`：给出写成没有。
    pub fn try_set_named(&mut self, target: Value<'gc>, key: &str, value: Value<'gc>) -> Result<bool, Error> {
        if let (Value::Object(o), Some(i)) = (target, index_of_str(key))
            && o.borrow_mut(self.mc).write_index(i, value)
        {
            return Ok(true);
        }
        self.set_key(target, Key::Str(key), value, target)
    }

    /// `target.key = value`：写不成抛 TypeError（规范里内建函数的 Set(…, true)）。
    pub fn set_named(&mut self, target: Value<'gc>, key: &str, value: Value<'gc>) -> Result<(), Error> {
        if self.try_set_named(target, key, value)? { Ok(()) } else { Err(self.set_failure(target, key)) }
    }

    /// `target[key] = value`，给出写成没有。
    pub fn try_set_keyed(&mut self, target: Value<'gc>, key: Value<'gc>, value: Value<'gc>) -> Result<bool, Error> {
        if let Value::Symbol(k) = key {
            return self.try_set_symbol(target, k, value);
        }
        if let (Value::Object(o), Value::Number(x)) = (target, key)
            && let Some(i) = index_of_number(x)
            && o.borrow_mut(self.mc).write_index(i, value)
        {
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

    /// `target[key] = value`：写不成抛 TypeError。
    pub fn set_keyed(&mut self, target: Value<'gc>, key: Value<'gc>, value: Value<'gc>) -> Result<(), Error> {
        if self.try_set_keyed(target, key, value)? {
            Ok(())
        } else {
            let k = match key {
                Value::Symbol(s) => self.symbol_descriptive(s),
                k => self.to_string(k)?,
            };
            Err(self.read_only(&k))
        }
    }

    /// JS 代码里的赋值：严格模式写不成抛 TypeError，非严格悄悄丢掉。
    pub fn put_named(&mut self, target: Value<'gc>, key: &str, value: Value<'gc>, strict: bool) -> Result<(), Error> {
        if strict { self.set_named(target, key, value) } else { self.try_set_named(target, key, value).map(drop) }
    }

    pub fn put_keyed(&mut self, target: Value<'gc>, key: Value<'gc>, value: Value<'gc>, strict: bool) -> Result<(), Error> {
        if strict { self.set_keyed(target, key, value) } else { self.try_set_keyed(target, key, value).map(drop) }
    }

    /// 定义自有属性（对象字面量、类、CreateDataPropertyOrThrow）：定义不成抛 TypeError。
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

    /// 定义访问器的一半（另一半已有就保留）。
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

    /// `key in target`（含原型链与数组下标）。
    pub fn has_property(&mut self, target: Value<'gc>, key: &str) -> Result<bool, Error> {
        let Value::Object(o) = target else {
            let what = self.to_string(target)?;
            return Err(self.throw(ErrorKind::Type, &format!("Cannot use 'in' operator to search for '{key}' in {}", &**what)));
        };
        self.has_key(o, Key::Str(key))
    }

    /// `delete target.key`：不可配置的删不掉。
    pub fn delete(&mut self, target: Value<'gc>, key: &str) -> Result<bool, Error> {
        self.delete_key(target, Key::Str(key))
    }

    /// CanBeHeldWeakly：对象，或者没在 `Symbol.for` 登记过的 Symbol。
    pub fn can_be_held_weakly(&self, v: Value<'gc>) -> bool {
        match v {
            Value::Object(_) => true,
            Value::Symbol(s) => !self.realm.symbol_registry.borrow().iter().any(|(_, x)| Gc::ptr_eq(*x, s)),
            _ => false,
        }
    }

    /// 报错信息里的值：不调用户代码（ToString 可观察，报错不能用）。
    pub fn describe(&self, v: Value<'gc>) -> String {
        match v {
            Value::Undefined | Value::Uninit => String::from("undefined"),
            Value::Null => String::from("null"),
            Value::Bool(b) => String::from(if b { "true" } else { "false" }),
            Value::Number(x) => String::from(&*number_text(x)),
            Value::String(s) => format!("\"{}\"", &**s),
            Value::Symbol(s) => String::from(&**self.symbol_descriptive(s)),
            Value::BigInt(b) => format!("{}n", b.0),
            Value::Object(o) => {
                let d = o.borrow();
                if d.kind.is_callable() {
                    match d.get_own("name").and_then(|p| p.value()) {
                        Some(Value::String(n)) if !n.is_empty() => String::from(&**n),
                        _ => String::from("function"),
                    }
                } else if matches!(d.kind, ObjectKind::Array(_)) {
                    String::from("[object Array]")
                } else {
                    String::from("#<Object>")
                }
            }
        }
    }

    /// `v instanceof f`：先看 `f[Symbol.hasInstance]`，再按 OrdinaryHasInstance。
    pub fn instance_of(&mut self, v: Value<'gc>, f: Value<'gc>) -> Result<bool, Error> {
        if !matches!(f, Value::Object(_)) {
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

    /// OrdinaryHasInstance：绑定函数换成目标，再沿 `v` 的原型链找 `f.prototype`。
    pub fn ordinary_has_instance(&mut self, f: Value<'gc>, v: Value<'gc>) -> Result<bool, Error> {
        if !self.is_callable(f) {
            return Ok(false);
        }
        if let Value::Object(o) = f
            && let ObjectKind::Bound(b) = &o.borrow().kind
        {
            let target = b.target;
            return self.instance_of(v, target);
        }
        let Value::Object(o) = v else { return Ok(false) };
        let Value::Object(proto) = self.get_named(f, "prototype")? else {
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

    /// ToIntegerOrInfinity。
    pub fn to_integer_or_infinity(&mut self, v: Value<'gc>) -> Result<f64, Error> {
        let x = self.to_number(v)?;
        Ok(if x.is_nan() { 0.0 } else { libm::trunc(x) + 0.0 })
    }

    /// ToLength：夹到 `[0, 2^53 − 1]`。
    pub fn to_length(&mut self, v: Value<'gc>) -> Result<f64, Error> {
        let n = self.to_integer_or_infinity(v)?;
        Ok(n.clamp(0.0, MAX_SAFE_INTEGER))
    }

    /// LengthOfArrayLike。
    pub fn length_of_array_like(&mut self, o: Value<'gc>) -> Result<f64, Error> {
        let len = self.get_named(o, "length")?;
        self.to_length(len)
    }

    /// 数下标 ⇒ 属性键文本。
    pub fn index_key(i: f64) -> String {
        String::from(&*number_text(i))
    }

    /// `o[i]`（数组稠密部分走快路）。
    pub fn get_index(&mut self, o: Value<'gc>, i: f64) -> Result<Value<'gc>, Error> {
        if let Value::Object(x) = o
            && matches!(x.borrow().kind, ObjectKind::TypedArray(..))
        {
            return Ok(self.typed_get(x, i).unwrap_or_default());
        }
        if let Value::Object(x) = o
            && let (ObjectKind::Array(items), Some(n)) = (&x.borrow().kind, index_of_number(i))
            && let Some(&v) = items.get(n as usize)
        {
            return Ok(v);
        }
        self.get_named(o, &Self::index_key(i))
    }

    /// `o[i] = v`，写不成抛 TypeError（Set(O, P, V, true)）。
    pub fn set_index(&mut self, o: Value<'gc>, i: f64, v: Value<'gc>) -> Result<(), Error> {
        if let (Value::Object(x), Some(n)) = (o, index_of_number(i))
            && x.borrow_mut(self.mc).write_index(n, v)
        {
            return Ok(());
        }
        self.set_named(o, &Self::index_key(i), v)
    }

    /// HasProperty(O, i)。
    pub fn has_index(&mut self, o: JsObject<'gc>, i: f64) -> Result<bool, Error> {
        if let (ObjectKind::Array(items), Some(n)) = (&o.borrow().kind, index_of_number(i))
            && (n as usize) < items.len()
        {
            return Ok(true);
        }
        self.has_key(o, Key::Str(&Self::index_key(i)))
    }

    /// DeletePropertyOrThrow(O, i)。
    pub fn delete_index(&mut self, o: JsObject<'gc>, i: f64) -> Result<(), Error> {
        let k = Self::index_key(i);
        if self.delete_property(o, Key::Str(&k))? {
            Ok(())
        } else {
            Err(self.throw(ErrorKind::Type, &format!("Cannot delete property '{k}' of {}", "[object Array]")))
        }
    }

    /// CreateDataPropertyOrThrow(O, i, v)。
    pub fn create_index(&mut self, o: JsObject<'gc>, i: f64, v: Value<'gc>) -> Result<(), Error> {
        // 快路：真数组的稠密部分里改值，或正好接在稠密末尾（可扩展、length 可写、后面没有稀疏元素）追加
        if let Some(n) = index_of_number(i) {
            let mut d = o.borrow_mut(self.mc);
            let append = !d.non_extensible && !d.length_readonly && d.sparse_length <= n;
            if let ObjectKind::Array(items) = &mut d.kind {
                let at = n as usize;
                if at < items.len() {
                    items[at] = v;
                    return Ok(());
                }
                if at == items.len() && append {
                    items.push(v);
                    return Ok(());
                }
            }
        }
        self.create_data_property_or_throw(o, Key::Str(&Self::index_key(i)), v)
    }

    /// IsArray。
    pub fn is_array(&mut self, v: Value<'gc>) -> Result<bool, Error> {
        let Value::Object(o) = v else { return Ok(false) };
        if let Some(parts) = self.proxy_parts(o) {
            let (t, _) = parts?;
            return self.is_array(t.into());
        }
        Ok(matches!(o.borrow().kind, ObjectKind::Array(_)))
    }

    /// CreateListFromArrayLike。
    pub fn list_from_array_like(&mut self, v: Value<'gc>) -> Result<Vec<Value<'gc>>, Error> {
        if !matches!(v, Value::Object(_)) {
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

    /// GetIterator：调 `v[Symbol.iterator]()`，取它的 `next`。
    pub fn get_iterator(&mut self, v: Value<'gc>) -> Result<IterRecord<'gc>, Error> {
        let method = if v.is_nullish() { Value::Undefined } else { self.get_symbol(v, self.realm.symbols.iterator, v)? };
        if !self.is_callable(method) {
            let what = match v {
                Value::Symbol(s) => self.symbol_descriptive(s),
                _ => self.to_string(v)?,
            };
            return Err(self.throw(ErrorKind::Type, &format!("{} is not iterable", &**what)));
        }
        let iterator = self.call(method, v, &[])?;
        if !matches!(iterator, Value::Object(_)) {
            return Err(self.throw(ErrorKind::Type, "Result of the Symbol.iterator method is not an object"));
        }
        let next = self.get_named(iterator, "next")?;
        Ok(IterRecord { iterator, next })
    }

    /// 走一步：完了给 `None`。
    pub fn iterator_step(&mut self, it: IterRecord<'gc>) -> Result<Option<Value<'gc>>, Error> {
        let r = self.call(it.next, it.iterator, &[])?;
        if !matches!(r, Value::Object(_)) {
            let what = self.to_string(r)?;
            return Err(self.throw(ErrorKind::Type, &format!("Iterator result {} is not an object", &**what)));
        }
        let done = self.get_named(r, "done")?;
        if self.to_boolean(done) { Ok(None) } else { Ok(Some(self.get_named(r, "value")?)) }
    }

    /// 提前结束迭代：有 `return` 就调它。
    pub fn iterator_close(&mut self, iterator: Value<'gc>) -> Result<(), Error> {
        let ret = self.get_named(iterator, "return")?;
        if ret.is_nullish() {
            return Ok(());
        }
        let r = self.call(ret, iterator, &[])?;
        if !matches!(r, Value::Object(_)) {
            return Err(self.throw(ErrorKind::Type, "Iterator result is not an object"));
        }
        Ok(())
    }

    /// 迭代结果对象 `{ value, done }`。
    pub fn iter_result(&self, value: Value<'gc>, done: bool) -> JsObject<'gc> {
        let o = self.object();
        let mut d = o.borrow_mut(self.mc);
        d.set("value", value);
        d.set("done", done.into());
        drop(d);
        o
    }

    /// 内建迭代器对象（数组 / 字符串）。
    pub fn builtin_iterator(&self, state: IteratorState<'gc>) -> JsObject<'gc> {
        let proto = match state {
            IteratorState::Array { .. } => self.realm.array_iterator_proto,
            IteratorState::String { .. } => self.realm.string_iterator_proto,
        };
        ObjectData { kind: ObjectKind::Iterator(state), proto: Some(proto), ..ObjectData::default() }.alloc(self.mc)
    }

    /// 数组迭代器（`values` / `keys` / `entries`）。
    pub fn array_iterator(&self, target: Value<'gc>, kind: IterKind) -> JsObject<'gc> {
        self.builtin_iterator(IteratorState::Array { target: Some(target), index: 0, kind })
    }

    /// 展开 / 剩余元素：把可迭代对象走完。
    pub fn to_list(&mut self, v: Value<'gc>) -> Result<Vec<Value<'gc>>, Error> {
        let it = self.get_iterator(v)?;
        let mut out = Vec::new();
        while let Some(x) = self.iterator_step(it)? {
            out.push(x);
        }
        Ok(out)
    }

    /// 对象展开 / 剩余（CopyDataProperties）：把 `src` 的自有可枚举属性定义到 `target` 上，跳过 `exclude` 里的键。
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

    /// `for-in` 要走的键：自有在前、再沿原型链，可枚举的字符串键，去重（被不可枚举属性遮住的也不出）。
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

    /// 构造调用要建的 `this`：原型取 `new_target.prototype`，不是对象就用 `Object.prototype`。
    pub fn create_this(&mut self, new_target: Value<'gc>) -> Result<JsObject<'gc>, Error> {
        let proto = match self.get_named(new_target, "prototype")? {
            Value::Object(p) => p,
            _ => self.realm.object_proto,
        };
        Ok(ObjectData { proto: Some(proto), ..ObjectData::default() }.alloc(self.mc))
    }
}
