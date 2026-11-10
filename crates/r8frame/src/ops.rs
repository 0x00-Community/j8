//! JS 规范里的运算语义（各执行层共用同一份）：类型转换、运算符、属性存取。
use alloc::boxed::Box;
use alloc::format;
use alloc::string::String;
use alloc::vec::Vec;

use gc_arena::Gc;

use num_traits::Zero;

use crate::{Agent, Error, Numeric, IterKind, IteratorState, JsObject, JsString, JsSymbol, ObjectData, ObjectKind, Property, Slot, Value};

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

/// 一个迭代器记录：迭代器对象与它的 `next` 方法。
#[derive(Clone, Copy, Debug)]
pub struct IterRecord<'gc> {
    pub iterator: Value<'gc>,
    pub next: Value<'gc>,
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
pub(crate) fn index_of_str(s: &str) -> Option<u32> {
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
        e.borrow_mut(self.mc).define(self.mc, "message", Property::hidden(m.into()));
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
            Value::Undefined | Value::Null | Value::Uninit => false,
            Value::Bool(b) => b,
            Value::Number(x) => !(x == 0.0 || x.is_nan()),
            Value::String(s) => !s.is_empty(),
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
            Value::Number(x) => self.string(ryu_js::Buffer::new().format(x)),
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
        // 加法：两边先 ToPrimitive 看有没有字符串；其余运算左边整个 ToNumeric 完再轮到右边（求值顺序看得见）
        let (l, r) = if op == BinaryOp::Add {
            let (l, r) = (self.to_primitive(lhs, Hint::Default)?, self.to_primitive(rhs, Hint::Default)?);
            if matches!(l, Value::String(_)) || matches!(r, Value::String(_)) {
                let (a, b) = (self.to_string(l)?, self.to_string(r)?);
                return Ok(self.string(&[&**a, &**b].concat()).into());
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
        let (x, y) = (modulo_u32(a).cast_signed(), modulo_u32(b));
        Ok(match op {
            BinaryOp::Add => a + b,
            BinaryOp::Sub => a - b,
            BinaryOp::Mul => a * b,
            BinaryOp::Div => a / b,
            BinaryOp::Rem => a % b,
            BinaryOp::Exp => pow(a, b),
            BinaryOp::BitAnd => f64::from(x & y.cast_signed()),
            BinaryOp::BitOr => f64::from(x | y.cast_signed()),
            BinaryOp::BitXor => f64::from(x ^ y.cast_signed()),
            BinaryOp::Shl => f64::from(x.wrapping_shl(y & 31)),
            BinaryOp::Shr => f64::from(x.wrapping_shr(y & 31)),
            BinaryOp::UShr => f64::from(x.cast_unsigned() >> (y & 31)),
        }
        .into())
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

    /// 沿原型链找属性（不含数组 / 字符串的下标与 `length`）。
    fn lookup(start: Option<JsObject<'gc>>, key: &str) -> Option<Property<'gc>> {
        let mut cur = start;
        while let Some(c) = cur {
            let d = c.borrow();
            if let Some(p) = d.get_own(key) {
                return Some(p);
            }
            cur = d.proto;
        }
        None
    }

    /// 数组 / 字符串的下标与 `length`。
    fn exotic(&self, target: Value<'gc>, key: &str) -> Option<Value<'gc>> {
        let o = match target {
            Value::String(s) => {
                return match (key, index_of_str(key)) {
                    ("length", _) => Some(f64::from(u32::try_from(s.encode_utf16().count()).unwrap_or(u32::MAX)).into()),
                    (_, Some(i)) => s.encode_utf16().nth(i as usize).map(|u| self.string(&String::from_utf16_lossy(&[u])).into()),
                    _ => None,
                };
            }
            Value::Object(o) => o,
            _ => return None,
        };
        let d = o.borrow();
        match (&d.kind, key, index_of_str(key)) {
            // String 包装对象的下标与 `length` 同字符串
            (ObjectKind::Primitive(s @ Value::String(_)), _, _) => self.exotic(*s, key),
            (ObjectKind::Array(_), "length", _) => d.array_length().map(|n| f64::from(n).into()),
            // 稠密部分之外的下标是普通属性（稀疏元素），交给属性查找
            (ObjectKind::Array(items), _, Some(i)) => items.get(i as usize).copied(),
            _ => None,
        }
    }

    fn nullish_error(&self, target: Value<'gc>, verb: &str, key: &str) -> Error {
        let what = if target == Value::Undefined { "undefined" } else { "null" };
        self.throw(ErrorKind::Type, &format!("Cannot {verb} properties of {what} (reading '{key}')"))
    }

    /// `holder` 上（沿原型链）读 `key`，getter 的 `this` 是 `receiver`。
    pub fn get(&mut self, holder: Value<'gc>, key: &str, receiver: Value<'gc>) -> Result<Value<'gc>, Error> {
        if holder.is_nullish() {
            return Err(self.nullish_error(holder, "read", key));
        }
        if let Some(v) = self.exotic(holder, key) {
            return Ok(v);
        }
        match Self::lookup(self.proto_of(holder), key).map(|p| p.slot) {
            Some(Slot::Data(v)) => Ok(v),
            Some(Slot::Accessor { get: Some(g), .. }) => self.call(g.into(), receiver, &[]),
            None | Some(Slot::Accessor { get: None, .. }) => Ok(Value::Undefined),
        }
    }

    /// `target.key`。
    pub fn get_named(&mut self, target: Value<'gc>, key: &str) -> Result<Value<'gc>, Error> {
        self.get(target, key, target)
    }

    /// 属性键：ToString（数组下标的数快路在调用处）。
    pub fn property_key(&mut self, key: Value<'gc>) -> Result<JsString<'gc>, Error> {
        self.to_string(key)
    }

    /// 沿原型链找 Symbol 键的属性。
    fn lookup_symbol(start: Option<JsObject<'gc>>, key: JsSymbol<'gc>) -> Option<Property<'gc>> {
        let mut cur = start;
        while let Some(c) = cur {
            let d = c.borrow();
            if let Some(p) = d.get_own_symbol(key) {
                return Some(p);
            }
            cur = d.proto;
        }
        None
    }

    /// `holder[sym]`，getter 的 `this` 是 `receiver`。
    pub fn get_symbol(&mut self, holder: Value<'gc>, key: JsSymbol<'gc>, receiver: Value<'gc>) -> Result<Value<'gc>, Error> {
        if holder.is_nullish() {
            let what = self.symbol_descriptive(key);
            return Err(self.nullish_error(holder, "read", &what));
        }
        match Self::lookup_symbol(self.proto_of(holder), key).map(|p| p.slot) {
            Some(Slot::Data(v)) => Ok(v),
            Some(Slot::Accessor { get: Some(g), .. }) => self.call(g.into(), receiver, &[]),
            None | Some(Slot::Accessor { get: None, .. }) => Ok(Value::Undefined),
        }
    }

    /// `target[sym] = value`。
    pub fn set_symbol(&mut self, target: Value<'gc>, key: JsSymbol<'gc>, value: Value<'gc>) -> Result<(), Error> {
        let o = match target {
            Value::Undefined | Value::Null => {
                let what = self.symbol_descriptive(key);
                return Err(self.nullish_error(target, "set", &what));
            }
            Value::Object(o) => o,
            _ => return Ok(()),
        };
        match Self::lookup_symbol(Some(o), key) {
            Some(Property { slot: Slot::Accessor { set: Some(s), .. }, .. }) => {
                self.call(s.into(), target, &[value])?;
            }
            Some(Property { slot: Slot::Accessor { set: None, .. }, .. } | Property { writable: false, .. }) => {}
            Some(_) if o.borrow().get_own_symbol(key).is_some() => {
                let mut d = o.borrow_mut(self.mc);
                if let Some((_, p)) = d.symbols.iter_mut().find(|(k, _)| Gc::ptr_eq(*k, key)) {
                    p.slot = Slot::Data(value);
                }
            }
            _ => o.borrow_mut(self.mc).define_symbol(key, Property::data(value)),
        }
        Ok(())
    }

    /// `key in target`，键是任意值。
    pub fn has_keyed(&mut self, target: Value<'gc>, key: Value<'gc>) -> Result<bool, Error> {
        if let Value::Symbol(k) = key {
            let Value::Object(o) = target else {
                return Err(self.throw(ErrorKind::Type, "Cannot use 'in' operator to search for a symbol in a non-object"));
            };
            return Ok(Self::lookup_symbol(Some(o), k).is_some());
        }
        let k = self.property_key(key)?;
        self.has_property(target, &k)
    }

    /// `delete target[key]`，键是任意值。
    pub fn delete_keyed(&mut self, target: Value<'gc>, key: Value<'gc>) -> Result<bool, Error> {
        if let (Value::Symbol(k), Value::Object(o)) = (key, target) {
            return Ok(o.borrow_mut(self.mc).remove_symbol(k));
        }
        let k = self.property_key(key)?;
        self.delete(target, &k)
    }

    /// 定义属性，键是任意值（计算键）。
    pub fn define_keyed(&mut self, o: JsObject<'gc>, key: Value<'gc>, prop: Property<'gc>) -> Result<(), Error> {
        if let Value::Symbol(k) = key {
            o.borrow_mut(self.mc).define_symbol(k, prop);
            return Ok(());
        }
        let k = self.property_key(key)?;
        self.define(o, &k, prop);
        Ok(())
    }

    /// 定义访问器的一半，键是任意值。
    pub fn define_accessor_keyed(&mut self, o: JsObject<'gc>, key: Value<'gc>, f: JsObject<'gc>, setter: bool, enumerable: bool) -> Result<(), Error> {
        if let Value::Symbol(k) = key {
            let (mut get, mut set) = match o.borrow().get_own_symbol(k).map(|p| p.slot) {
                Some(Slot::Accessor { get, set }) => (get, set),
                _ => (None, None),
            };
            if setter { set = Some(f) } else { get = Some(f) }
            o.borrow_mut(self.mc).define_symbol(k, Property { slot: Slot::Accessor { get, set }, enumerable, writable: true, configurable: true });
            return Ok(());
        }
        let k = self.property_key(key)?;
        self.define_accessor(o, &k, f, setter, enumerable);
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
        let k = self.property_key(key)?;
        self.get_named(target, &k)
    }

    /// 数组元素写（稠密 / 稀疏见 [`ObjectData::write_index`]）。是数组下标就写了返回 `true`。
    fn set_element(&self, o: JsObject<'gc>, key: &str, value: Value<'gc>) -> bool {
        index_of_str(key).is_some_and(|i| o.borrow_mut(self.mc).write_index(self.mc, i, value))
    }

    /// `target.key = value`：原型链上有 setter 就调它，有不可写的数据属性就不写；原始值上的写悄悄丢掉。
    pub fn set_named(&mut self, target: Value<'gc>, key: &str, value: Value<'gc>) -> Result<(), Error> {
        let o = match target {
            Value::Undefined | Value::Null => return Err(self.nullish_error(target, "set", key)),
            Value::Object(o) => o,
            _ => return Ok(()),
        };
        if self.set_element(o, key, value) {
            return Ok(());
        }
        if key == "length" && matches!(o.borrow().kind, ObjectKind::Array(_)) {
            let n = self.to_uint32(value)?;
            if f64::from(n) != self.to_number(value)? {
                return Err(self.throw(ErrorKind::Range, "Invalid array length"));
            }
            o.borrow_mut(self.mc).set_array_length(n);
            return Ok(());
        }
        match Self::lookup(Some(o), key) {
            Some(Property { slot: Slot::Accessor { set: Some(s), .. }, .. }) => {
                self.call(s.into(), target, &[value])?;
            }
            Some(Property { slot: Slot::Accessor { set: None, .. }, .. } | Property { writable: false, .. }) => {}
            _ => o.borrow_mut(self.mc).set(self.mc, key, value),
        }
        Ok(())
    }

    /// `target[key] = value`。
    pub fn set_keyed(&mut self, target: Value<'gc>, key: Value<'gc>, value: Value<'gc>) -> Result<(), Error> {
        if let Value::Symbol(k) = key {
            return self.set_symbol(target, k, value);
        }
        if let (Value::Object(o), Value::Number(x)) = (target, key)
            && let Some(i) = index_of_number(x)
            && o.borrow_mut(self.mc).write_index(self.mc, i, value)
        {
            return Ok(());
        }
        let k = self.property_key(key)?;
        self.set_named(target, &k, value)
    }

    /// 定义自有属性（对象字面量、类）：数组下标写元素。
    pub fn define(&mut self, o: JsObject<'gc>, key: &str, prop: Property<'gc>) {
        if let (Slot::Data(v), true) = (prop.slot, prop.enumerable)
            && self.set_element(o, key, v)
        {
            return;
        }
        o.borrow_mut(self.mc).define(self.mc, key, prop);
    }

    /// 定义访问器的一半（另一半已有就保留）。
    pub fn define_accessor(&mut self, o: JsObject<'gc>, key: &str, f: JsObject<'gc>, setter: bool, enumerable: bool) {
        let (mut get, mut set) = match o.borrow().get_own(key).map(|p| p.slot) {
            Some(Slot::Accessor { get, set }) => (get, set),
            _ => (None, None),
        };
        if setter { set = Some(f) } else { get = Some(f) }
        o.borrow_mut(self.mc).define(self.mc, key, Property { slot: Slot::Accessor { get, set }, enumerable, writable: true, configurable: true });
    }

    /// `key in target`（含原型链与数组下标）。
    pub fn has_property(&mut self, target: Value<'gc>, key: &str) -> Result<bool, Error> {
        let Value::Object(o) = target else {
            let what = self.to_string(target)?;
            return Err(self.throw(ErrorKind::Type, &format!("Cannot use 'in' operator to search for '{key}' in {}", &**what)));
        };
        if let ObjectKind::Array(items) = &o.borrow().kind {
            match (key, index_of_str(key)) {
                ("length", _) => return Ok(true),
                (_, Some(i)) if (i as usize) < items.len() => return Ok(true),
                _ => {}
            }
        }
        Ok(Self::lookup(Some(o), key).is_some())
    }

    /// `delete target[key]`：数组下标置 `undefined`（还没有空洞），不可配置的删不掉。
    pub fn delete(&mut self, target: Value<'gc>, key: &str) -> Result<bool, Error> {
        match target {
            Value::Undefined | Value::Null => Err(self.nullish_error(target, "delete", key)),
            Value::Object(o) => {
                let mut data = o.borrow_mut(self.mc);
                if let (ObjectKind::Array(items), Some(i)) = (&mut data.kind, index_of_str(key))
                    && let Some(x) = items.get_mut(i as usize)
                {
                    *x = Value::Undefined;
                    return Ok(true);
                }
                Ok(data.remove(key))
            }
            _ => Ok(true),
        }
    }

    /// `v instanceof f`：先看 `f[Symbol.hasInstance]`，再沿 `v` 的原型链找 `f.prototype`（绑定函数换成目标）。
    pub fn instance_of(&mut self, v: Value<'gc>, f: Value<'gc>) -> Result<bool, Error> {
        if matches!(f, Value::Object(_)) {
            let h = self.get_symbol(f, self.realm.symbols.has_instance, f)?;
            if !h.is_nullish() {
                let r = self.call(h, f, &[v])?;
                return Ok(self.to_boolean(r));
            }
        }
        if !self.is_callable(f) {
            return Err(self.throw(ErrorKind::Type, "Right-hand side of 'instanceof' is not callable"));
        }
        if let Value::Object(o) = f
            && let ObjectKind::Bound(b) = &o.borrow().kind
        {
            let target = b.target;
            return self.instance_of(v, target);
        }
        let Value::Object(proto) = self.get_named(f, "prototype")? else {
            return Err(self.throw(ErrorKind::Type, "Function has non-object prototype in instanceof check"));
        };
        let Value::Object(o) = v else { return Ok(false) };
        let mut cur = o.borrow().proto;
        while let Some(c) = cur {
            if Gc::ptr_eq(c, proto) {
                return Ok(true);
            }
            cur = c.borrow().proto;
        }
        Ok(false)
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
        d.set(self.mc, "value", value);
        d.set(self.mc, "done", done.into());
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

    /// 对象展开 / 剩余：把 `src` 的自有可枚举属性（数组下标在前）定义到 `target` 上，跳过 `exclude` 里的键。
    pub fn copy_data(&mut self, target: JsObject<'gc>, src: Value<'gc>, exclude: &[JsString<'gc>]) -> Result<(), Error> {
        let skip = |k: &str| exclude.iter().any(|e| ***e == *k);
        let keys: Vec<JsString<'gc>> = match src {
            Value::Object(o) => {
                let data = o.borrow();
                let mut keys = Vec::new();
                if let ObjectKind::Array(items) = &data.kind {
                    keys.extend((0..items.len()).map(|i| self.string(ryu_js::Buffer::new().format(f64::from(u32::try_from(i).unwrap_or(u32::MAX))))));
                }
                keys.extend(data.props.iter().filter(|(_, p)| p.enumerable).map(|&(k, _)| k));
                keys
            }
            Value::String(s) => (0..s.encode_utf16().count()).map(|i| self.string(ryu_js::Buffer::new().format(f64::from(u32::try_from(i).unwrap_or(u32::MAX))))).collect(),
            _ => return Ok(()),
        };
        for k in keys.into_iter().filter(|k| !skip(k)) {
            let v = self.get_named(src, &k)?;
            self.define(target, &k, Property::data(v));
        }
        if let Value::Object(o) = src {
            let syms: Vec<JsSymbol<'gc>> = o.borrow().symbols.iter().filter(|(_, p)| p.enumerable).map(|&(k, _)| k).collect();
            for k in syms {
                let v = self.get_symbol(src, k, src)?;
                target.borrow_mut(self.mc).define_symbol(k, Property::data(v));
            }
        }
        Ok(())
    }

    /// `for-in` 要走的键：自有在前、再沿原型链，可枚举的字符串键，去重（被不可枚举属性遮住的也不出）。
    pub fn for_in_keys(&mut self, v: Value<'gc>) -> Result<Vec<Value<'gc>>, Error> {
        let index = |i: usize| ryu_js::Buffer::new().format(f64::from(u32::try_from(i).unwrap_or(u32::MAX))).into();
        let mut seen: Vec<alloc::boxed::Box<str>> = Vec::new();
        let mut out = Vec::new();
        let mut cur = match v {
            Value::Undefined | Value::Null => return Ok(out),
            Value::String(s) => {
                for i in 0..s.encode_utf16().count() {
                    let k: alloc::boxed::Box<str> = index(i);
                    out.push(self.string(&k).into());
                    seen.push(k);
                }
                Some(self.realm.string_proto)
            }
            other => self.proto_of(other),
        };
        while let Some(o) = cur {
            let d = o.borrow();
            if let ObjectKind::Array(items) = &d.kind {
                for i in 0..items.len() {
                    let k: alloc::boxed::Box<str> = index(i);
                    if !seen.contains(&k) {
                        out.push(self.string(&k).into());
                        seen.push(k);
                    }
                }
            }
            for (k, p) in &d.props {
                if seen.iter().any(|s| **s == ***k) {
                    continue;
                }
                seen.push((&***k).into());
                if p.enumerable {
                    out.push(Value::String(*k));
                }
            }
            cur = d.proto;
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
