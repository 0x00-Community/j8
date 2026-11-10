//! JS 规范里的运算语义（各执行层共用同一份）：类型转换、运算符、属性存取。
use alloc::boxed::Box;
use alloc::format;
use alloc::string::String;
use alloc::vec::Vec;

use gc_arena::Gc;

use crate::{Agent, Error, JsObject, JsString, ObjectData, ObjectKind, Value};

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
}

/// ToPrimitive 的偏好。
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Hint {
    Default,
    Number,
    String,
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
    pub const ALL: [ErrorKind; 5] = [ErrorKind::Error, ErrorKind::Type, ErrorKind::Reference, ErrorKind::Range, ErrorKind::Syntax];

    pub fn name(self) -> &'static str {
        match self {
            ErrorKind::Error => "Error",
            ErrorKind::Type => "TypeError",
            ErrorKind::Reference => "ReferenceError",
            ErrorKind::Range => "RangeError",
            ErrorKind::Syntax => "SyntaxError",
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

impl<'gc> Agent<'gc, '_> {
    /// 字符串放上堆。
    pub fn string(&self, s: &str) -> JsString<'gc> {
        Gc::new(self.mc, Box::from(s))
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
        let e = ObjectData { proto: Some(self.realm.error_proto(kind)), ..ObjectData::default() }.alloc(self.mc);
        let m = self.string(message);
        e.borrow_mut(self.mc).set(self.mc, "message", m.into());
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

    pub fn to_boolean(&self, v: Value<'gc>) -> bool {
        match v {
            Value::Undefined | Value::Null => false,
            Value::Bool(b) => b,
            Value::Number(x) => !(x == 0.0 || x.is_nan()),
            Value::String(s) => !s.is_empty(),
            Value::Object(_) => true,
        }
    }

    /// ToPrimitive：对象按偏好依次试 `valueOf` / `toString`，结果是原始值就用。
    pub fn to_primitive(&mut self, v: Value<'gc>, hint: Hint) -> Result<Value<'gc>, Error> {
        if !matches!(v, Value::Object(_)) {
            return Ok(v);
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
            Value::Undefined => f64::NAN,
            Value::Null => 0.0,
            Value::Bool(b) => f64::from(u8::from(b)),
            Value::Number(x) => x,
            Value::String(s) => parse_number(&s),
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
            Value::Undefined => self.string("undefined"),
            Value::Null => self.string("null"),
            Value::Bool(true) => self.string("true"),
            Value::Bool(false) => self.string("false"),
            Value::Number(x) => self.string(ryu_js::Buffer::new().format(x)),
            Value::Object(_) => {
                let p = self.to_primitive(v, Hint::String)?;
                return self.to_string(p);
            }
        })
    }

    pub fn type_of(&self, v: Value<'gc>) -> &'static str {
        match v {
            Value::Undefined => "undefined",
            Value::Bool(_) => "boolean",
            Value::Number(_) => "number",
            Value::String(_) => "string",
            Value::Object(o) if o.borrow().kind.is_callable() => "function",
            Value::Null | Value::Object(_) => "object",
        }
    }

    pub fn binary(&mut self, op: BinaryOp, lhs: Value<'gc>, rhs: Value<'gc>) -> Result<Value<'gc>, Error> {
        if op == BinaryOp::Add {
            let (l, r) = (self.to_primitive(lhs, Hint::Default)?, self.to_primitive(rhs, Hint::Default)?);
            if matches!(l, Value::String(_)) || matches!(r, Value::String(_)) {
                let (a, b) = (self.to_string(l)?, self.to_string(r)?);
                return Ok(self.string(&[&**a, &**b].concat()).into());
            }
            return Ok((self.to_number(l)? + self.to_number(r)?).into());
        }
        if matches!(op, BinaryOp::BitAnd | BinaryOp::BitOr | BinaryOp::BitXor | BinaryOp::Shl | BinaryOp::Shr | BinaryOp::UShr) {
            let (x, y) = (self.to_int32(lhs)?, self.to_uint32(rhs)?);
            let r = match op {
                BinaryOp::BitAnd => f64::from(x & y.cast_signed()),
                BinaryOp::BitOr => f64::from(x | y.cast_signed()),
                BinaryOp::BitXor => f64::from(x ^ y.cast_signed()),
                BinaryOp::Shl => f64::from(x.wrapping_shl(y & 31)),
                BinaryOp::Shr => f64::from(x.wrapping_shr(y & 31)),
                _ => f64::from(x.cast_unsigned() >> (y & 31)),
            };
            return Ok(r.into());
        }
        let (a, b) = (self.to_number(lhs)?, self.to_number(rhs)?);
        Ok(match op {
            BinaryOp::Sub => a - b,
            BinaryOp::Mul => a * b,
            BinaryOp::Div => a / b,
            BinaryOp::Rem => a % b,
            _ => pow(a, b),
        }
        .into())
    }

    /// 宽松相等（`==`）。
    pub fn loose_equals(&mut self, a: Value<'gc>, b: Value<'gc>) -> Result<bool, Error> {
        Ok(match (a, b) {
            _ if a.is_nullish() && b.is_nullish() => true,
            (Value::Undefined | Value::Null, _) | (_, Value::Undefined | Value::Null) => false,
            (Value::Object(_), Value::Object(_)) => a == b,
            (Value::Object(_), _) => {
                let p = self.to_primitive(a, Hint::Default)?;
                return self.loose_equals(p, b);
            }
            (_, Value::Object(_)) => {
                let p = self.to_primitive(b, Hint::Default)?;
                return self.loose_equals(a, p);
            }
            (Value::Number(_), Value::String(_)) | (Value::String(_), Value::Number(_)) | (Value::Bool(_), _) | (_, Value::Bool(_)) => self.to_number(a)? == self.to_number(b)?,
            _ => a == b,
        })
    }

    pub fn compare(&mut self, op: CompareOp, lhs: Value<'gc>, rhs: Value<'gc>) -> Result<bool, Error> {
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
                let (a, b) = (self.to_number(l)?, self.to_number(r)?);
                match op {
                    CompareOp::Lt => a < b,
                    CompareOp::Gt => a > b,
                    CompareOp::Le => a <= b,
                    _ => a >= b,
                }
            }
        })
    }

    pub fn unary(&mut self, op: UnaryOp, v: Value<'gc>) -> Result<Value<'gc>, Error> {
        Ok(match op {
            UnaryOp::Neg => (-self.to_number(v)?).into(),
            UnaryOp::Plus => self.to_number(v)?.into(),
            UnaryOp::Not => (!self.to_boolean(v)).into(),
            UnaryOp::BitNot => f64::from(!self.to_int32(v)?).into(),
            UnaryOp::Typeof => self.string(self.type_of(v)).into(),
            UnaryOp::Void => Value::Undefined,
        })
    }

    /// 原始值读属性时用的原型。
    fn proto_of(&self, v: Value<'gc>) -> Option<JsObject<'gc>> {
        match v {
            Value::String(_) => Some(self.realm.string_proto),
            Value::Number(_) => Some(self.realm.number_proto),
            Value::Bool(_) => Some(self.realm.boolean_proto),
            Value::Object(o) => Some(o),
            Value::Undefined | Value::Null => None,
        }
    }

    /// `target.key`：数组下标与 `length`、字符串下标与 `length`、沿原型链找属性。
    pub fn get_named(&mut self, target: Value<'gc>, key: &str) -> Result<Value<'gc>, Error> {
        match target {
            Value::Undefined | Value::Null => {
                let what = if target == Value::Undefined { "undefined" } else { "null" };
                return Err(self.throw(ErrorKind::Type, &format!("Cannot read properties of {what} (reading '{key}')")));
            }
            Value::String(s) => match (key, index_of_str(key)) {
                ("length", _) => return Ok(f64::from(u32::try_from(s.encode_utf16().count()).unwrap_or(u32::MAX)).into()),
                (_, Some(i)) => return Ok(s.encode_utf16().nth(i as usize).map_or(Value::Undefined, |u| self.string(&String::from_utf16_lossy(&[u])).into())),
                _ => {}
            },
            Value::Object(o) => {
                if let ObjectKind::Array(items) = &o.borrow().kind {
                    match (key, index_of_str(key)) {
                        ("length", _) => return Ok(f64::from(u32::try_from(items.len()).unwrap_or(u32::MAX)).into()),
                        (_, Some(i)) => return Ok(items.get(i as usize).copied().unwrap_or_default()),
                        _ => {}
                    }
                }
            }
            Value::Bool(_) | Value::Number(_) => {}
        }
        let mut cur = self.proto_of(target);
        while let Some(c) = cur {
            let d = c.borrow();
            if let Some(v) = d.own(key) {
                return Ok(v);
            }
            cur = d.proto;
        }
        Ok(Value::Undefined)
    }

    /// 属性键：ToString（数组下标的数快路在调用处）。
    fn key(&mut self, key: Value<'gc>) -> Result<JsString<'gc>, Error> {
        self.to_string(key)
    }

    /// `target[key]`。
    pub fn get_keyed(&mut self, target: Value<'gc>, key: Value<'gc>) -> Result<Value<'gc>, Error> {
        if let (Value::Object(o), Value::Number(x)) = (target, key)
            && let (ObjectKind::Array(items), Some(i)) = (&o.borrow().kind, index_of_number(x))
        {
            return Ok(items.get(i as usize).copied().unwrap_or_default());
        }
        let k = self.key(key)?;
        self.get_named(target, &k)
    }

    /// `target.key = value`：数组下标写元素（越过末尾补 `undefined`），原始值上的写悄悄丢掉。
    pub fn set_named(&mut self, target: Value<'gc>, key: &str, value: Value<'gc>) -> Result<(), Error> {
        match target {
            Value::Undefined | Value::Null => {
                let what = if target == Value::Undefined { "undefined" } else { "null" };
                Err(self.throw(ErrorKind::Type, &format!("Cannot set properties of {what} (setting '{key}')")))
            }
            Value::Object(o) => {
                let mut data = o.borrow_mut(self.mc);
                if let ObjectKind::Array(items) = &mut data.kind {
                    if let Some(i) = index_of_str(key) {
                        let i = i as usize;
                        if i >= items.len() {
                            items.resize(i + 1, Value::Undefined);
                        }
                        items[i] = value;
                        return Ok(());
                    }
                    if key == "length" {
                        drop(data);
                        let n = self.to_uint32(value)? as usize;
                        if let ObjectKind::Array(items) = &mut o.borrow_mut(self.mc).kind {
                            items.resize(n, Value::Undefined);
                        }
                        return Ok(());
                    }
                }
                data.set(self.mc, key, value);
                Ok(())
            }
            _ => Ok(()),
        }
    }

    /// `target[key] = value`。
    pub fn set_keyed(&mut self, target: Value<'gc>, key: Value<'gc>, value: Value<'gc>) -> Result<(), Error> {
        if let (Value::Object(o), Value::Number(x)) = (target, key)
            && let Some(i) = index_of_number(x)
            && let ObjectKind::Array(items) = &mut o.borrow_mut(self.mc).kind
        {
            let i = i as usize;
            if i >= items.len() {
                items.resize(i + 1, Value::Undefined);
            }
            items[i] = value;
            return Ok(());
        }
        let k = self.key(key)?;
        self.set_named(target, &k, value)
    }
}
