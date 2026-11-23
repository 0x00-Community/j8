use j8::frame::{Scope, JsString};
use j8::{Bytes, BufferRef};

pub struct Raw;

#[derive(Clone, Copy, PartialEq, Eq)]
enum Encoding {
    Ascii,
    Utf8,
    Base64,
    Ucs2,
    Latin1,
    Hex,
    Buffer,
    Base64Url,
}

impl TryFrom<u32> for Encoding {
    type Error = ();

    fn try_from(n: u32) -> Result<Self, ()> {
        const ALL: [Encoding; 8] = [Encoding::Ascii, Encoding::Utf8, Encoding::Base64, Encoding::Ucs2, Encoding::Latin1, Encoding::Hex, Encoding::Buffer, Encoding::Base64Url];
        usize::try_from(n).ok().and_then(|i| ALL.get(i).copied()).ok_or(())
    }
}

const HEX: &[u8; 16] = b"0123456789abcdef";
const BASE64: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
const BASE64URL: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789-_";
const ASCII_MASK: u8 = 0x7f;
const BYTE_MASK: u16 = 0xff;
const PAD: u8 = b'=';

struct Text<'t> {
    units: &'t [u16],
}

impl Text<'_> {
    fn chars(&self) -> impl Iterator<Item = char> + '_ {
        char::decode_utf16(self.units.iter().copied()).map(|c| c.unwrap_or(char::REPLACEMENT_CHARACTER))
    }

    fn utf8_len(&self) -> usize {
        self.chars().map(char::len_utf8).sum()
    }

    fn encode(&self, encoding: Encoding, limit: usize) -> Vec<u8> {
        let mut out = Vec::with_capacity(limit.min(self.units.len() * 3));
        match encoding {
            Encoding::Utf8 | Encoding::Buffer => {
                let mut buf = [0; 4];
                for c in self.chars() {
                    let s = c.encode_utf8(&mut buf);
                    if out.len() + s.len() > limit {
                        break;
                    }
                    out.extend_from_slice(s.as_bytes());
                }
            }
            Encoding::Ucs2 => out.extend(self.units.iter().flat_map(|u| u.to_le_bytes()).take(limit & !1)),
            Encoding::Ascii | Encoding::Latin1 => out.extend(self.units.iter().take(limit).map(|&u| (u & BYTE_MASK) as u8)),
            Encoding::Hex => {
                let digit = |u: u16| char::from_u32(u32::from(u)).and_then(|c| c.to_digit(16));
                for pair in self.units.as_chunks::<2>().0.iter().take(limit) {
                    let (Some(hi), Some(lo)) = (digit(pair[0]), digit(pair[1])) else { break };
                    out.extend(u8::try_from(hi << 4 | lo));
                }
            }
            Encoding::Base64 | Encoding::Base64Url => {
                let value = |u: u16| -> Option<u32> {
                    let b = u8::try_from(u).ok()?;
                    BASE64.iter().position(|&x| x == b).or_else(|| BASE64URL.iter().position(|&x| x == b)).and_then(|p| u32::try_from(p).ok())
                };
                let (mut acc, mut bits) = (0u32, 0);
                for &u in self.units {
                    if u == u16::from(PAD) {
                        break;
                    }
                    let Some(v) = value(u) else { continue };
                    acc = acc << 6 | v;
                    bits += 6;
                    if bits >= 8 {
                        bits -= 8;
                        if out.len() == limit {
                            break;
                        }
                        out.push((acc >> bits & 0xff) as u8);
                    }
                }
            }
        }
        out
    }
}

struct Data<'d> {
    bytes: &'d [u8],
}

impl Data<'_> {
    fn decode(&self, encoding: Encoding) -> Option<String> {
        Some(match encoding {
            Encoding::Ucs2 => return None,
            Encoding::Utf8 | Encoding::Buffer => String::from_utf8_lossy(self.bytes).into_owned(),
            Encoding::Latin1 => self.bytes.iter().map(|&b| char::from(b)).collect(),
            Encoding::Ascii => self.bytes.iter().map(|&b| char::from(b & ASCII_MASK)).collect(),
            Encoding::Hex => self.bytes.iter().flat_map(|&b| [HEX[usize::from(b >> 4)], HEX[usize::from(b & 0xf)]]).map(char::from).collect(),
            Encoding::Base64 => self.base64(BASE64, true),
            Encoding::Base64Url => self.base64(BASE64URL, false),
        })
    }

    fn base64(&self, alphabet: &[u8; 64], pad: bool) -> String {
        let mut out = String::with_capacity(self.bytes.len().div_ceil(3) * 4);
        for chunk in self.bytes.chunks(3) {
            let n = chunk.iter().enumerate().fold(0u32, |n, (i, &b)| n | u32::from(b) << (16 - 8 * i));
            for i in 0..4 {
                if i <= chunk.len() {
                    out.push(char::from(alphabet[(n >> (18 - 6 * i) & 63) as usize]));
                } else if pad {
                    out.push(char::from(PAD));
                }
            }
        }
        out
    }

    fn find(&self, needle: &[u8], from: usize, forward: bool, step: usize) -> Option<usize> {
        let last = self.bytes.len().checked_sub(needle.len())?;
        let hit = |i: &usize| self.bytes[*i..*i + needle.len()] == *needle;
        if forward {
            (from..=last).step_by(step).find(hit)
        } else {
            let start = from.min(last);
            let start = start - start % step;
            (0..=start).rev().step_by(step).find(hit)
        }
    }
}

struct Ordering(std::cmp::Ordering);

impl From<Ordering> for i32 {
    fn from(o: Ordering) -> Self {
        match o.0 {
            std::cmp::Ordering::Less => -1,
            std::cmp::Ordering::Equal => 0,
            std::cmp::Ordering::Greater => 1,
        }
    }
}

#[j8::object("raw")]
impl Raw {
    fn decode<'scope>(a: &mut Scope<'scope, '_>, buf: BufferRef<'scope>, start: usize, end: usize, encoding: u32) -> Option<String> {
        let encoding = Encoding::try_from(encoding).ok()?;
        buf.with(a, |b| b.get(start..end).and_then(|bytes| Data { bytes }.decode(encoding))).flatten()
    }

    fn encode(text: JsString<'_>, encoding: u32) -> Option<Bytes> {
        let encoding = Encoding::try_from(encoding).ok()?;
        let units = text.units().to_vec();
        Some(Bytes(Text { units: &units }.encode(encoding, usize::MAX)))
    }

    fn write<'scope>(a: &mut Scope<'scope, '_>, buf: BufferRef<'scope>, text: JsString<'scope>, offset: usize, max: usize, encoding: u32) -> Option<usize> {
        let encoding = Encoding::try_from(encoding).ok()?;
        let units = text.units().to_vec();
        let bytes = Text { units: &units }.encode(encoding, max);
        buf.with(a, |b| {
            let room = b.get_mut(offset..)?;
            let n = bytes.len().min(room.len());
            room[..n].copy_from_slice(&bytes[..n]);
            Some(n)
        })
        .flatten()
    }

    fn utf8_length(text: JsString<'_>) -> usize {
        let units = text.units().to_vec();
        Text { units: &units }.utf8_len()
    }

    fn copy<'scope>(a: &mut Scope<'scope, '_>, source: BufferRef<'scope>, target: BufferRef<'scope>, target_start: usize, source_start: usize, count: usize) -> usize {
        let Some(data) = source.with(a, |b| b.get(source_start..source_start + count).map(<[u8]>::to_vec)).flatten() else { return 0 };
        target.with(a, |b| b.get_mut(target_start..target_start + data.len()).map(|t| t.copy_from_slice(&data))).flatten().map_or(0, |()| data.len())
    }

    fn compare<'scope>(a: &mut Scope<'scope, '_>, x: BufferRef<'scope>, y: BufferRef<'scope>, x_start: usize, x_end: usize, y_start: usize, y_end: usize) -> i32 {
        let left = x.with(a, |b| b.get(x_start..x_end).map(<[u8]>::to_vec)).flatten().unwrap_or_default();
        y.with(a, |b| b.get(y_start..y_end).map(|right| i32::from(Ordering(left.as_slice().cmp(right))))).flatten().unwrap_or(0)
    }

    fn fill<'scope>(a: &mut Scope<'scope, '_>, buf: BufferRef<'scope>, pattern: Bytes, start: usize, end: usize) -> bool {
        if pattern.0.is_empty() {
            return false;
        }
        buf.with(a, |b| b.get_mut(start..end).map(|t| t.iter_mut().zip(pattern.0.iter().cycle()).for_each(|(d, s)| *d = *s))).flatten().is_some()
    }

    fn index_of<'scope>(a: &mut Scope<'scope, '_>, haystack: BufferRef<'scope>, needle: Bytes, from: usize, forward: bool, wide: bool) -> f64 {
        let step = if wide { 2 } else { 1 };
        let found = haystack.with(a, |b| {
            let len = if wide { b.len() & !1 } else { b.len() };
            Data { bytes: &b[..len] }.find(&needle.0, from, forward, step)
        });
        found.flatten().and_then(|i| u32::try_from(i).ok()).map_or(-1.0, f64::from)
    }

    fn swap<'scope>(a: &mut Scope<'scope, '_>, buf: BufferRef<'scope>, width: usize) {
        buf.with(a, |b| b.chunks_exact_mut(width).for_each(<[u8]>::reverse));
    }

    fn is_utf8<'scope>(a: &mut Scope<'scope, '_>, buf: BufferRef<'scope>) -> Option<bool> {
        buf.with(a, |b| std::str::from_utf8(b).is_ok())
    }

    fn is_ascii<'scope>(a: &mut Scope<'scope, '_>, buf: BufferRef<'scope>) -> Option<bool> {
        buf.with(a, |b| b.is_ascii())
    }
}
