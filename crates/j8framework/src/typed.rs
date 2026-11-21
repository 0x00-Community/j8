use alloc::vec::Vec;

use num_bigint::BigInt;
use num_traits::ToPrimitive;

use crate::{Agent, ElementKind, Error, JsObject, Key, ObjectKind, Value, ViewData};

const TWO_32: f64 = 4_294_967_296.0;

const F16_BIAS: i32 = 15;

const F16_MANTISSA: u32 = 10;

const F16_EXP_MAX: u16 = 0x1f;

#[derive(Clone, Debug)]
pub enum Element {
    Number(f64),
    BigInt(BigInt),
}

fn modulo(x: f64, bits: u32) -> u64 {
    if !x.is_finite() {
        return 0;
    }
    let t = libm::trunc(x);
    let m = if bits == 32 { TWO_32 } else { f64::from(1u32 << bits) };
    let r = t - libm::floor(t / m) * m;
    #[expect(clippy::cast_possible_truncation, clippy::cast_sign_loss, reason = "already reduced modulo to [0, 2^bits)")]
    let r = r as u64;
    r
}

fn clamp_u8(x: f64) -> u8 {
    if x.is_nan() || x <= 0.0 {
        return 0;
    }
    if x >= 255.0 {
        return 255;
    }
    let f = libm::floor(x);
    let up = f + 0.5 < x || (f + 0.5 == x && f % 2.0 != 0.0);
    let r = if up { f + 1.0 } else { f };
    #[expect(clippy::cast_possible_truncation, clippy::cast_sign_loss, reason = "already clamped to [0, 255]")]
    let r = r as u8;
    r
}

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
    if a >= 65520.0 {
        return sign | (F16_EXP_MAX << F16_MANTISSA);
    }
    let e = libm::floor(libm::log2(a));
    #[expect(clippy::cast_possible_truncation, reason = "the exponent is in [-1075, 16]")]
    let mut e = e as i32;
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
    #[expect(clippy::cast_possible_truncation, clippy::cast_sign_loss, reason = "the mantissa is at most 2^11")]
    let m = m as u32;
    if !normal {
        #[expect(clippy::cast_possible_truncation, reason = "at most 2^10")]
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
    #[expect(clippy::cast_possible_truncation, clippy::cast_sign_loss, reason = "exponent in [1, 30], mantissa 10 bits")]
    let r = sign | ((exp as u16) << F16_MANTISSA) | ((mant & ((1 << F16_MANTISSA) - 1)) as u16);
    r
}

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
    pub fn encode(self, v: &Element) -> Vec<u8> {
        match (self, v) {
            (ElementKind::BigInt64 | ElementKind::BigUint64, Element::BigInt(b)) => {
                let w = Agent::wrap_bigint(b, 64, false);
                w.to_u64().unwrap_or(0).to_le_bytes().to_vec()
            }
            (_, Element::Number(x)) => {
                let x = *x;
                match self {
                    #[expect(clippy::cast_possible_truncation, reason = "already reduced modulo")]
                    ElementKind::Int8 | ElementKind::Uint8 => alloc::vec![modulo(x, 8) as u8],
                    ElementKind::Uint8Clamped => alloc::vec![clamp_u8(x)],
                    #[expect(clippy::cast_possible_truncation, reason = "already reduced modulo")]
                    ElementKind::Int16 | ElementKind::Uint16 => (modulo(x, 16) as u16).to_le_bytes().to_vec(),
                    #[expect(clippy::cast_possible_truncation, reason = "already reduced modulo")]
                    ElementKind::Int32 | ElementKind::Uint32 => (modulo(x, 32) as u32).to_le_bytes().to_vec(),
                    ElementKind::Float16 => f64_to_f16(x).to_le_bytes().to_vec(),
                    #[expect(clippy::cast_possible_truncation, reason = "Float32 is meant to round")]
                    ElementKind::Float32 => (x as f32).to_le_bytes().to_vec(),
                    ElementKind::Float64 => x.to_le_bytes().to_vec(),
                    ElementKind::BigInt64 | ElementKind::BigUint64 => alloc::vec![0; 8],
                }
            }
            (_, Element::BigInt(_)) => alloc::vec![0; self.size()],
        }
    }

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
    pub fn buffer_len(buffer: JsObject<'gc>) -> Option<usize> {
        match &buffer.borrow().kind {
            ObjectKind::ArrayBuffer(b) => b.bytes.as_deref().map(<[u8]>::len),
            _ => None,
        }
    }

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

    pub fn typed_array(o: JsObject<'gc>) -> Option<(ElementKind, ViewData<'gc>)> {
        match o.borrow().kind {
            ObjectKind::TypedArray(k, v) => Some((k, v)),
            _ => None,
        }
    }

    pub fn typed_length(o: JsObject<'gc>) -> usize {
        Self::typed_array(o).and_then(|(k, v)| Self::view_length(&v, k.size())).unwrap_or(0)
    }

    fn valid_index(o: JsObject<'gc>, i: f64) -> Option<usize> {
        let (k, v) = Self::typed_array(o)?;
        if i != libm::trunc(i) || (i == 0.0 && i.is_sign_negative()) || i < 0.0 {
            return None;
        }
        let len = Self::view_length(&v, k.size())?;
        #[expect(clippy::cast_possible_truncation, clippy::cast_sign_loss, reason = "checked to be a non-negative integer")]
        let n = i as usize;
        (i < 9_007_199_254_740_992.0 && n < len).then_some(n)
    }

    pub fn element_value(&self, e: Element) -> Value<'gc> {
        match e {
            Element::Number(x) => x.into(),
            Element::BigInt(b) => self.bigint(b),
        }
    }

    pub fn typed_get(&self, o: JsObject<'gc>, i: f64) -> Option<Value<'gc>> {
        let n = Self::valid_index(o, i)?;
        let (k, v) = Self::typed_array(o)?;
        let e = Self::read_raw(v.buffer, v.offset + n * k.size(), k)?;
        Some(self.element_value(e))
    }

    pub fn read_raw(buffer: JsObject<'gc>, at: usize, k: ElementKind) -> Option<Element> {
        match &buffer.borrow().kind {
            ObjectKind::ArrayBuffer(b) => b.bytes.as_ref().and_then(|bytes| bytes.get(at..at + k.size())).map(|s| k.decode(s)),
            _ => None,
        }
    }

    pub fn write_raw(&self, buffer: JsObject<'gc>, at: usize, k: ElementKind, e: &Element) {
        let bytes = k.encode(e);
        if let ObjectKind::ArrayBuffer(b) = &mut buffer.borrow_mut(self.mc).kind
            && let Some(dst) = b.bytes.as_mut().and_then(|x| x.get_mut(at..at + bytes.len()))
        {
            dst.copy_from_slice(&bytes);
        }
    }

    pub fn to_element(&mut self, k: ElementKind, v: Value<'gc>) -> Result<Element, Error> {
        if k.is_bigint() { Ok(Element::BigInt(self.to_bigint(v)?)) } else { Ok(Element::Number(self.to_number(v)?)) }
    }

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

    pub(crate) fn typed_numeric(o: JsObject<'gc>, key: Key<'_, 'gc>) -> Option<f64> {
        let Key::Str(k) = key else { return None };
        if o.is_plain() || !matches!(o.borrow().kind, ObjectKind::TypedArray(..)) {
            return None;
        }
        canonical_numeric(k)
    }

    pub(crate) fn typed_has(o: JsObject<'gc>, i: f64) -> bool {
        Self::valid_index(o, i).is_some()
    }
}
