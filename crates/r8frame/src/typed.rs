//! 二进制数据：元素的编解码（含 Float16）、TypedArray 的长度与越界、整数下标特殊对象的属性行为（规范 10.4.5）。
use alloc::vec::Vec;

use num_bigint::BigInt;
use num_traits::ToPrimitive;

use crate::{Agent, ElementKind, Error, JsObject, Key, ObjectKind, Value, ViewData};

/// 2^32
const TWO_32: f64 = 4_294_967_296.0;
/// Float16 的指数偏置
const F16_BIAS: i32 = 15;
/// Float16 尾数位数
const F16_MANTISSA: u32 = 10;
/// Float16 指数全 1（无穷 / NaN）
const F16_EXP_MAX: u16 = 0x1f;

/// 要写进缓冲的数：TypedArray 先按元素类型转好（ToNumber / ToBigInt）再查下标。
#[derive(Clone, Debug)]
pub enum Element {
    Number(f64),
    BigInt(BigInt),
}

/// 浮点数 ⇒ 2^bits 取模后的无符号整数（ToInt8 / ToUint16 … 的公共部分）。
fn modulo(x: f64, bits: u32) -> u64 {
    if !x.is_finite() {
        return 0;
    }
    let t = libm::trunc(x);
    let m = if bits == 32 { TWO_32 } else { f64::from(1u32 << bits) };
    let r = t - libm::floor(t / m) * m;
    #[expect(clippy::cast_possible_truncation, clippy::cast_sign_loss, reason = "已取模到 [0, 2^bits)")]
    let r = r as u64;
    r
}

/// ToUint8Clamp：夹到 [0, 255]，一半时取偶数。
fn clamp_u8(x: f64) -> u8 {
    if x.is_nan() || x <= 0.0 {
        return 0;
    }
    if x >= 255.0 {
        return 255;
    }
    let f = libm::floor(x);
    // 比一半大进位，正好一半时往偶数靠
    let up = f + 0.5 < x || (f + 0.5 == x && f % 2.0 != 0.0);
    let r = if up { f + 1.0 } else { f };
    #[expect(clippy::cast_possible_truncation, clippy::cast_sign_loss, reason = "已夹到 [0, 255]")]
    let r = r as u8;
    r
}

/// f64 ⇒ Float16 位（就近舍入、一半取偶，一次舍入不经过 f32）。
pub fn f64_to_f16(x: f64) -> u16 {
    let bits = x.to_bits();
    let sign = u16::from(bits >> 63 != 0) << 15;
    if x.is_nan() {
        return sign | (F16_EXP_MAX << F16_MANTISSA) | 0x200;
    }
    let a = x.abs();
    if a == 0.0 {
        return sign;
    }
    // 最大有限值 65504，舍入到无穷的分界是 65520
    if a >= 65520.0 {
        return sign | (F16_EXP_MAX << F16_MANTISSA);
    }
    // 2^-24 是最小的非规格数；按 2^-24 为单位就近取偶
    let e = libm::floor(libm::log2(a));
    #[expect(clippy::cast_possible_truncation, reason = "指数在 [-1075, 16] 里")]
    let mut e = e as i32;
    // log2 可能差一，校正
    if libm::ldexp(1.0, e) > a {
        e -= 1;
    } else if libm::ldexp(1.0, e + 1) <= a {
        e += 1;
    }
    let (unit_exp, normal) = if e < 1 - F16_BIAS { (-24, false) } else { (e - F16_MANTISSA.cast_signed(), true) };
    let scaled = libm::ldexp(a, -unit_exp);
    let mut m = libm::floor(scaled);
    let frac = scaled - m;
    if frac > 0.5 || (frac == 0.5 && m % 2.0 != 0.0) {
        m += 1.0;
    }
    #[expect(clippy::cast_possible_truncation, clippy::cast_sign_loss, reason = "尾数不超过 2^11")]
    let m = m as u32;
    if !normal {
        // 非规格数（舍入后可能进成最小规格数，位模式正好连续）
        #[expect(clippy::cast_possible_truncation, reason = "不超过 2^10")]
        return sign | m as u16;
    }
    let mut exp = e + F16_BIAS;
    let mut mant = m;
    if mant >= 1 << (F16_MANTISSA + 1) {
        mant >>= 1;
        exp += 1;
    }
    if exp >= i32::from(F16_EXP_MAX) {
        return sign | (F16_EXP_MAX << F16_MANTISSA);
    }
    #[expect(clippy::cast_possible_truncation, clippy::cast_sign_loss, reason = "指数在 [1, 30]、尾数 10 位")]
    let r = sign | ((exp as u16) << F16_MANTISSA) | ((mant & ((1 << F16_MANTISSA) - 1)) as u16);
    r
}

/// Float16 位 ⇒ f64。
pub fn f16_to_f64(h: u16) -> f64 {
    let sign = if h & 0x8000 != 0 { -1.0 } else { 1.0 };
    let exp = (h >> F16_MANTISSA) & F16_EXP_MAX;
    let mant = f64::from(h & ((1 << F16_MANTISSA) - 1));
    match exp {
        0 => sign * libm::ldexp(mant, -24),
        F16_EXP_MAX if mant == 0.0 => sign * f64::INFINITY,
        F16_EXP_MAX => f64::NAN,
        _ => sign * libm::ldexp(1024.0 + mant, i32::from(exp) - F16_BIAS - 10),
    }
}

impl ElementKind {
    /// 编码成小端字节。
    pub fn encode(self, v: &Element) -> Vec<u8> {
        match (self, v) {
            (ElementKind::BigInt64 | ElementKind::BigUint64, Element::BigInt(b)) => {
                let w = Agent::wrap_bigint(b, 64, false);
                w.to_u64().unwrap_or(0).to_le_bytes().to_vec()
            }
            (_, Element::Number(x)) => {
                let x = *x;
                match self {
                    #[expect(clippy::cast_possible_truncation, reason = "已取模")]
                    ElementKind::Int8 | ElementKind::Uint8 => alloc::vec![modulo(x, 8) as u8],
                    ElementKind::Uint8Clamped => alloc::vec![clamp_u8(x)],
                    #[expect(clippy::cast_possible_truncation, reason = "已取模")]
                    ElementKind::Int16 | ElementKind::Uint16 => (modulo(x, 16) as u16).to_le_bytes().to_vec(),
                    #[expect(clippy::cast_possible_truncation, reason = "已取模")]
                    ElementKind::Int32 | ElementKind::Uint32 => (modulo(x, 32) as u32).to_le_bytes().to_vec(),
                    ElementKind::Float16 => f64_to_f16(x).to_le_bytes().to_vec(),
                    #[expect(clippy::cast_possible_truncation, reason = "Float32 就是要舍入")]
                    ElementKind::Float32 => (x as f32).to_le_bytes().to_vec(),
                    ElementKind::Float64 => x.to_le_bytes().to_vec(),
                    ElementKind::BigInt64 | ElementKind::BigUint64 => alloc::vec![0; 8],
                }
            }
            (_, Element::BigInt(_)) => alloc::vec![0; self.size()],
        }
    }

    /// 从小端字节解出来。
    pub fn decode(self, b: &[u8]) -> Element {
        let arr = |n: usize| {
            let mut x = [0u8; 8];
            x[..n].copy_from_slice(&b[..n]);
            x
        };
        match self {
            ElementKind::Int8 => Element::Number(f64::from(i8::from_le_bytes([b[0]]))),
            ElementKind::Uint8 | ElementKind::Uint8Clamped => Element::Number(f64::from(b[0])),
            ElementKind::Int16 => Element::Number(f64::from(i16::from_le_bytes([b[0], b[1]]))),
            ElementKind::Uint16 => Element::Number(f64::from(u16::from_le_bytes([b[0], b[1]]))),
            ElementKind::Int32 => Element::Number(f64::from(i32::from_le_bytes([b[0], b[1], b[2], b[3]]))),
            ElementKind::Uint32 => Element::Number(f64::from(u32::from_le_bytes([b[0], b[1], b[2], b[3]]))),
            ElementKind::Float16 => Element::Number(f16_to_f64(u16::from_le_bytes([b[0], b[1]]))),
            ElementKind::Float32 => Element::Number(f64::from(f32::from_le_bytes([b[0], b[1], b[2], b[3]]))),
            ElementKind::Float64 => Element::Number(f64::from_le_bytes(arr(8))),
            ElementKind::BigInt64 => Element::BigInt(BigInt::from(i64::from_le_bytes(arr(8)))),
            ElementKind::BigUint64 => Element::BigInt(BigInt::from(u64::from_le_bytes(arr(8)))),
        }
    }
}

/// CanonicalNumericIndexString：`"-0"` 或者 ToString(ToNumber(s)) 正好是它自己。
pub fn canonical_numeric(key: &str) -> Option<f64> {
    if key == "-0" {
        return Some(-0.0);
    }
    let first = key.as_bytes().first()?;
    if !(first.is_ascii_digit() || *first == b'-' || key == "Infinity" || key == "NaN") {
        return None;
    }
    let n = crate::ops::parse_number(key);
    (ryu_js::Buffer::new().format(n) == key).then_some(n)
}

impl<'gc> Agent<'gc, '_> {
    /// 缓冲的字节数（分离了给 `None`）。
    pub fn buffer_len(buffer: JsObject<'gc>) -> Option<usize> {
        match &buffer.borrow().kind {
            ObjectKind::ArrayBuffer(b) => b.bytes.as_ref().map(Vec::len),
            _ => None,
        }
    }

    /// 视图（TypedArray 按元素、DataView 按字节）当前的长度：越界 / 分离给 `None`。
    pub fn view_length(view: &ViewData<'gc>, size: usize) -> Option<usize> {
        let len = Self::buffer_len(view.buffer)?;
        if view.offset > len {
            return None;
        }
        match view.length {
            Some(n) => (view.offset + n * size <= len).then_some(n),
            None => Some((len - view.offset) / size),
        }
    }

    /// 是 TypedArray 就给出元素类型与视图。
    pub fn typed_array(o: JsObject<'gc>) -> Option<(ElementKind, ViewData<'gc>)> {
        match o.borrow().kind {
            ObjectKind::TypedArray(k, v) => Some((k, v)),
            _ => None,
        }
    }

    /// TypedArray 的长度（越界给 0）。
    pub fn typed_length(o: JsObject<'gc>) -> usize {
        Self::typed_array(o).and_then(|(k, v)| Self::view_length(&v, k.size())).unwrap_or(0)
    }

    /// IsValidIntegerIndex：整数、不是 -0、在长度内。
    fn valid_index(o: JsObject<'gc>, i: f64) -> Option<usize> {
        let (k, v) = Self::typed_array(o)?;
        if i != libm::trunc(i) || (i == 0.0 && i.is_sign_negative()) || i < 0.0 {
            return None;
        }
        let len = Self::view_length(&v, k.size())?;
        #[expect(clippy::cast_possible_truncation, clippy::cast_sign_loss, reason = "已检查是非负整数")]
        let n = i as usize;
        (i < 9_007_199_254_740_992.0 && n < len).then_some(n)
    }

    /// 元素 ⇒ JS 值。
    pub fn element_value(&self, e: Element) -> Value<'gc> {
        match e {
            Element::Number(x) => x.into(),
            Element::BigInt(b) => self.bigint(b),
        }
    }

    /// 读 TypedArray 的第 `i` 个元素（下标不合法给 `None`）。
    pub fn typed_get(&self, o: JsObject<'gc>, i: f64) -> Option<Value<'gc>> {
        let n = Self::valid_index(o, i)?;
        let (k, v) = Self::typed_array(o)?;
        let e = Self::read_raw(v.buffer, v.offset + n * k.size(), k)?;
        Some(self.element_value(e))
    }

    /// 缓冲第 `at` 字节起读一个元素。
    pub fn read_raw(buffer: JsObject<'gc>, at: usize, k: ElementKind) -> Option<Element> {
        match &buffer.borrow().kind {
            ObjectKind::ArrayBuffer(b) => b.bytes.as_ref().and_then(|bytes| bytes.get(at..at + k.size())).map(|s| k.decode(s)),
            _ => None,
        }
    }

    /// 缓冲第 `at` 字节起写一个元素。
    pub fn write_raw(&self, buffer: JsObject<'gc>, at: usize, k: ElementKind, e: &Element) {
        let bytes = k.encode(e);
        if let ObjectKind::ArrayBuffer(b) = &mut buffer.borrow_mut(self.mc).kind
            && let Some(dst) = b.bytes.as_mut().and_then(|x| x.get_mut(at..at + bytes.len()))
        {
            dst.copy_from_slice(&bytes);
        }
    }

    /// 按元素类型转数（BigInt 类型 ToBigInt，别的 ToNumber）。
    pub fn to_element(&mut self, k: ElementKind, v: Value<'gc>) -> Result<Element, Error> {
        if k.is_bigint() { Ok(Element::BigInt(self.to_bigint(v)?)) } else { Ok(Element::Number(self.to_number(v)?)) }
    }

    /// TypedArraySetElement：先转数，下标合法才写。
    pub fn typed_set(&mut self, o: JsObject<'gc>, i: f64, v: Value<'gc>) -> Result<(), Error> {
        let Some((k, _)) = Self::typed_array(o) else { return Ok(()) };
        let e = self.to_element(k, v)?;
        if let Some(n) = Self::valid_index(o, i)
            && let Some((k, view)) = Self::typed_array(o)
        {
            self.write_raw(view.buffer, view.offset + n * k.size(), k, &e);
        }
        Ok(())
    }

    /// TypedArray 上的数字键：给出规范化后的下标。
    pub(crate) fn typed_numeric(o: JsObject<'gc>, key: Key<'_, 'gc>) -> Option<f64> {
        let Key::Str(k) = key else { return None };
        if !matches!(o.borrow().kind, ObjectKind::TypedArray(..)) {
            return None;
        }
        canonical_numeric(k)
    }

    pub(crate) fn typed_has(o: JsObject<'gc>, i: f64) -> bool {
        Self::valid_index(o, i).is_some()
    }
}
