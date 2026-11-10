//! JS 规范里的运算语义（各执行层共用同一份）：类型转换、运算符、属性存取。
use alloc::boxed::Box;
use alloc::format;
use alloc::string::String;
use alloc::vec::Vec;

use gc_arena::Gc;

use crate::{Error, JsString, ObjectKind, Scope, Value};

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

/// 抛出的内建错误种类。
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum ErrorKind {
    Type,
    Reference,
    Range,
}

impl BinaryOp {
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
}

impl CompareOp {
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
}

impl UnaryOp {
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
    pub fn name(self) -> &'static str {
        match self {
            ErrorKind::Type => "TypeError",
            ErrorKind::Reference => "ReferenceError",
            ErrorKind::Range => "RangeError",
        }
    }
}

/// 2^32：ToInt32 / ToUint32 的模。
const TWO_POW_32: f64 = 4_294_967_296.0;

/// 数组下标上限（`length` 最大 2^32 − 1）。
const MAX_INDEX: f64 = 4_294_967_294.0;

/// 字符串 ⇒ 数（StringToNumber）：前后空白、`Infinity`、`0x` / `0o` / `0b` 前缀、十进制。
fn parse_number(s: &str) -> f64 {
    let t = s.trim_matches(|c: char| c.is_whitespace() || c == '\u{feff}');
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
fn modulo_u32(x: f64) -> u32 {
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
fn index_of_str(s: &str) -> Option<u32> {
    let canonical = s == "0" || (!s.starts_with('0') && !s.is_empty() && s.bytes().all(|c| c.is_ascii_digit()));
    canonical.then(|| s.parse().ok()).flatten().filter(|&i| f64::from(i) <= MAX_INDEX)
}

impl<'gc> Scope<'gc, '_> {
    /// 字符串放上堆。
    pub fn string(&self, s: &str) -> JsString<'gc> {
        Gc::new(self.mc, Box::from(s))
    }

    /// 抛出一个内建错误（值是 `"TypeError: …"` 字符串，还没有 Error 对象）。
    pub fn throw(&self, kind: ErrorKind, message: &str) -> Error {
        let s = self.string(&format!("{}: {message}", kind.name()));
        Error::Exception(self.global(s.into()))
    }

    pub fn to_boolean(&self, v: Value<'gc>) -> bool {
        match v {
            Value::Undefined | Value::Null => false,
            Value::Bool(b) => b,
            Value::Number(x) => !(x == 0.0 || x.is_nan()),
            Value::String(s) => !s.is_empty(),
            Value::Object(_) => true,
        }
    }

    /// ToNumber。对象还没有 ToPrimitive，先当 NaN。
    pub fn to_number(&self, v: Value<'gc>) -> f64 {
        match v {
            Value::Undefined | Value::Object(_) => f64::NAN,
            Value::Null => 0.0,
            Value::Bool(b) => f64::from(u8::from(b)),
            Value::Number(x) => x,
            Value::String(s) => parse_number(&s),
        }
    }

    pub fn to_int32(&self, v: Value<'gc>) -> i32 {
        modulo_u32(self.to_number(v)).cast_signed()
    }

    pub fn to_uint32(&self, v: Value<'gc>) -> u32 {
        modulo_u32(self.to_number(v))
    }

    /// ToString。对象还没有 ToPrimitive，函数与普通对象给固定文本，数组按逗号连接。
    pub fn to_string(&self, v: Value<'gc>) -> JsString<'gc> {
        match v {
            Value::String(s) => s,
            Value::Undefined => self.string("undefined"),
            Value::Null => self.string("null"),
            Value::Bool(true) => self.string("true"),
            Value::Bool(false) => self.string("false"),
            Value::Number(x) => self.string(ryu_js::Buffer::new().format(x)),
            Value::Object(o) => match &o.borrow().kind {
                ObjectKind::Array(items) => {
                    let parts: Vec<String> = items.iter().map(|&e| if e.is_nullish() { String::new() } else { String::from(&**self.to_string(e)) }).collect();
                    self.string(&parts.join(","))
                }
                ObjectKind::Function(_) => self.string("function () { [native code] }"),
                ObjectKind::Ordinary => self.string("[object Object]"),
            },
        }
    }

    pub fn type_of(&self, v: Value<'gc>) -> &'static str {
        match v {
            Value::Undefined => "undefined",
            Value::Null => "object",
            Value::Bool(_) => "boolean",
            Value::Number(_) => "number",
            Value::String(_) => "string",
            Value::Object(o) => match o.borrow().kind {
                ObjectKind::Function(_) => "function",
                _ => "object",
            },
        }
    }

    pub fn binary(&self, op: BinaryOp, lhs: Value<'gc>, rhs: Value<'gc>) -> Value<'gc> {
        if op == BinaryOp::Add && (matches!(lhs, Value::String(_) | Value::Object(_)) || matches!(rhs, Value::String(_) | Value::Object(_))) {
            let (a, b) = (self.to_string(lhs), self.to_string(rhs));
            return self.string(&[&**a, &**b].concat()).into();
        }
        let (a, b) = (self.to_number(lhs), self.to_number(rhs));
        let int = |f: fn(i32, u32) -> i32| f64::from(f(self.to_int32(lhs), self.to_uint32(rhs)));
        match op {
            BinaryOp::Add => a + b,
            BinaryOp::Sub => a - b,
            BinaryOp::Mul => a * b,
            BinaryOp::Div => a / b,
            BinaryOp::Rem => a % b,
            BinaryOp::Exp => pow(a, b),
            BinaryOp::BitAnd => int(|x, y| x & y.cast_signed()),
            BinaryOp::BitOr => int(|x, y| x | y.cast_signed()),
            BinaryOp::BitXor => int(|x, y| x ^ y.cast_signed()),
            BinaryOp::Shl => int(|x, y| x.wrapping_shl(y & 31)),
            BinaryOp::Shr => int(|x, y| x.wrapping_shr(y & 31)),
            BinaryOp::UShr => f64::from(self.to_uint32(lhs) >> (self.to_uint32(rhs) & 31)),
        }
        .into()
    }

    /// 宽松相等（`==`）。对象对原始值还没有 ToPrimitive，按不等。
    pub fn loose_equals(&self, a: Value<'gc>, b: Value<'gc>) -> bool {
        match (a, b) {
            _ if a.is_nullish() && b.is_nullish() => true,
            (Value::Undefined | Value::Null, _) | (_, Value::Undefined | Value::Null) => false,
            (Value::Number(_), Value::String(_)) | (Value::String(_), Value::Number(_)) | (Value::Bool(_), _) | (_, Value::Bool(_)) => {
                if matches!(a, Value::Object(_)) || matches!(b, Value::Object(_)) {
                    return false;
                }
                self.to_number(a) == self.to_number(b)
            }
            _ => a == b,
        }
    }

    pub fn compare(&self, op: CompareOp, lhs: Value<'gc>, rhs: Value<'gc>) -> bool {
        match op {
            CompareOp::Eq => self.loose_equals(lhs, rhs),
            CompareOp::Ne => !self.loose_equals(lhs, rhs),
            CompareOp::StrictEq => lhs == rhs,
            CompareOp::StrictNe => lhs != rhs,
            CompareOp::Lt | CompareOp::Gt | CompareOp::Le | CompareOp::Ge => {
                if let (Value::String(a), Value::String(b)) = (lhs, rhs) {
                    // 按 UTF-16 码元比
                    let ord = a.encode_utf16().cmp(b.encode_utf16());
                    return match op {
                        CompareOp::Lt => ord.is_lt(),
                        CompareOp::Gt => ord.is_gt(),
                        CompareOp::Le => ord.is_le(),
                        _ => ord.is_ge(),
                    };
                }
                let (a, b) = (self.to_number(lhs), self.to_number(rhs));
                match op {
                    CompareOp::Lt => a < b,
                    CompareOp::Gt => a > b,
                    CompareOp::Le => a <= b,
                    _ => a >= b,
                }
            }
        }
    }

    pub fn unary(&self, op: UnaryOp, v: Value<'gc>) -> Value<'gc> {
        match op {
            UnaryOp::Neg => (-self.to_number(v)).into(),
            UnaryOp::Plus => self.to_number(v).into(),
            UnaryOp::Not => (!self.to_boolean(v)).into(),
            UnaryOp::BitNot => f64::from(!self.to_int32(v)).into(),
            UnaryOp::Typeof => self.string(self.type_of(v)).into(),
            UnaryOp::Void => Value::Undefined,
        }
    }

    /// `target.key`：数组下标与 `length`、字符串下标与 `length`、沿原型链找属性。
    pub fn get_named(&self, target: Value<'gc>, key: &str) -> Result<Value<'gc>, Error> {
        match target {
            Value::Undefined | Value::Null => Err(self.throw(ErrorKind::Type, &format!("cannot read property '{key}' of {}", &**self.to_string(target)))),
            Value::String(s) => Ok(match (key, index_of_str(key)) {
                ("length", _) => f64::from(u32::try_from(s.encode_utf16().count()).unwrap_or(u32::MAX)).into(),
                (_, Some(i)) => s.encode_utf16().nth(i as usize).map_or(Value::Undefined, |u| self.string(&String::from_utf16_lossy(&[u])).into()),
                _ => Value::Undefined,
            }),
            Value::Object(o) => {
                let data = o.borrow();
                if let ObjectKind::Array(items) = &data.kind {
                    match (key, index_of_str(key)) {
                        ("length", _) => return Ok(f64::from(u32::try_from(items.len()).unwrap_or(u32::MAX)).into()),
                        (_, Some(i)) => return Ok(items.get(i as usize).copied().unwrap_or_default()),
                        _ => {}
                    }
                }
                let mut cur = Some(o);
                while let Some(c) = cur {
                    let d = c.borrow();
                    if let Some(v) = d.own(key) {
                        return Ok(v);
                    }
                    cur = d.proto;
                }
                Ok(Value::Undefined)
            }
            Value::Bool(_) | Value::Number(_) => Ok(Value::Undefined),
        }
    }

    /// `target[key]`。
    pub fn get_keyed(&self, target: Value<'gc>, key: Value<'gc>) -> Result<Value<'gc>, Error> {
        if let (Value::Object(o), Value::Number(x)) = (target, key)
            && let (ObjectKind::Array(items), Some(i)) = (&o.borrow().kind, index_of_number(x))
        {
            return Ok(items.get(i as usize).copied().unwrap_or_default());
        }
        let k = self.to_string(key);
        self.get_named(target, &k)
    }

    /// `target.key = value`：数组下标写元素（越过末尾补 `undefined`），原始值上的写悄悄丢掉。
    pub fn set_named(&self, target: Value<'gc>, key: &str, value: Value<'gc>) -> Result<(), Error> {
        match target {
            Value::Undefined | Value::Null => Err(self.throw(ErrorKind::Type, &format!("cannot set property '{key}' of {}", &**self.to_string(target)))),
            Value::Object(o) => {
                let mut data = o.borrow_mut(self.mc);
                if let ObjectKind::Array(items) = &mut data.kind
                    && let Some(i) = index_of_str(key)
                {
                    let i = i as usize;
                    if i >= items.len() {
                        items.resize(i + 1, Value::Undefined);
                    }
                    items[i] = value;
                    return Ok(());
                }
                data.set(self.mc, key, value);
                Ok(())
            }
            _ => Ok(()),
        }
    }

    /// `target[key] = value`。
    pub fn set_keyed(&self, target: Value<'gc>, key: Value<'gc>, value: Value<'gc>) -> Result<(), Error> {
        let k = self.to_string(key);
        self.set_named(target, &k, value)
    }
}
