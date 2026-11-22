use alloc::sync::Arc;
use alloc::string::String;
use alloc::vec::Vec;
use core::cmp::Ordering;
use core::fmt;
use core::hash::{Hash, Hasher};
use core::sync::atomic::{AtomicPtr, Ordering as AtomicOrdering};

use j8lockfree::OnceBox;

use crate::{Collect};

const ROPE_MIN: usize = 128;

const INLINE: usize = 22;

const SLICE_MAX: usize = u16::MAX as usize;

#[derive(Clone, Collect)]
#[collect(require_static)]
pub struct SmallStr(Repr);

#[derive(Clone)]
enum Repr {
    Inline { len: u8, bytes: [u8; INLINE] },
    Heap(Arc<str>),
    Slice { len: u16, start: u32, buf: Arc<str> },
    Rope(Arc<Rope>),
}

const _: () = assert!(core::mem::size_of::<SmallStr>() == 24, "a string is three words");

struct Rope {
    len: usize,
    flat: OnceBox<SmallStr>,
    halves: Option<(SmallStr, SmallStr)>,
}

impl SmallStr {
    pub fn concat(a: &SmallStr, b: &SmallStr) -> SmallStr {
        let (la, lb) = (a.byte_len(), b.byte_len());
        if lb == 0 {
            return a.clone();
        }
        if la == 0 {
            return b.clone();
        }
        if la + lb <= INLINE
            && let Ok(len) = u8::try_from(la + lb)
        {
            let mut bytes = [0; INLINE];
            bytes[..la].copy_from_slice(a.as_str().as_bytes());
            bytes[la..la + lb].copy_from_slice(b.as_str().as_bytes());
            return Self(Repr::Inline { len, bytes });
        }
        if la + lb < ROPE_MIN {
            let mut s = String::with_capacity(la + lb);
            s.push_str(a);
            s.push_str(b);
            return Self::from(s.as_str());
        }
        Self(Repr::Rope(Arc::new(Rope { len: la + lb, flat: OnceBox::default(), halves: Some((a.clone(), b.clone())) })))
    }

    #[inline]
    pub fn byte_len(&self) -> usize {
        match &self.0 {
            Repr::Inline { len, .. } => usize::from(*len),
            Repr::Heap(s) => s.len(),
            Repr::Slice { len, .. } => usize::from(*len),
            Repr::Rope(r) => r.len,
        }
    }

    #[inline]
    pub fn as_str(&self) -> &str {
        match &self.0 {
            Repr::Inline { len, bytes } => unsafe { core::str::from_utf8_unchecked(&bytes[..usize::from(*len)]) },
            Repr::Heap(s) => s,
            Repr::Slice { len, start, buf } => unsafe { buf.get_unchecked(*start as usize..*start as usize + usize::from(*len)) },
            Repr::Rope(r) => r.flat.get_or_init(|| r.flatten()).as_str(),
        }
    }

    pub fn piece(&self, piece: &str) -> Option<SmallStr> {
        let text = self.as_str();
        let at = (piece.as_ptr() as usize).checked_sub(text.as_ptr() as usize)?;
        if at + piece.len() > text.len() {
            return None;
        }
        if piece.len() == text.len() {
            return Some(self.clone());
        }
        let (buf, base) = match &self.0 {
            _ if piece.len() <= INLINE || piece.len() > SLICE_MAX => return Some(Self::from(piece)),
            Repr::Heap(b) => (b, 0),
            Repr::Slice { start, buf, .. } => (buf, *start as usize),
            Repr::Rope(n) => match n.flat.get().map(|f| &f.0) {
                Some(Repr::Heap(b)) => (b, 0),
                _ => return Some(Self::from(piece)),
            },
            Repr::Inline { .. } => return Some(Self::from(piece)),
        };
        let (Ok(len), Ok(start)) = (u16::try_from(piece.len()), u32::try_from(base + at)) else { return Some(Self::from(piece)) };
        Some(Self(Repr::Slice { len, start, buf: buf.clone() }))
    }
}

impl Rope {
    fn flatten(&self) -> SmallStr {
        let mut out = String::with_capacity(self.len);
        let (l, r) = self.halves.clone().expect("a rope keeps its halves");
        let mut stack = alloc::vec![r, l];
        while let Some(s) = stack.pop() {
            match &s.0 {
                Repr::Rope(n) => match n.flat.get() {
                    Some(f) => out.push_str(f),
                    None => {
                        let (a, b) = n.halves.clone().expect("a rope keeps its halves");
                        stack.push(b);
                        stack.push(a);
                    }
                },
                _ => out.push_str(s.as_str()),
            }
        }
        SmallStr(Repr::Heap(Arc::from(out)))
    }
}

impl Drop for Rope {
    fn drop(&mut self) {
        let mut stack: Vec<SmallStr> = Vec::new();
        if let Some((l, r)) = self.halves.take() {
            stack.push(l);
            stack.push(r);
        }
        while let Some(s) = stack.pop() {
            if let Repr::Rope(rc) = s.0
                && let Ok(mut node) = Arc::try_unwrap(rc)
                && let Some((l, r)) = node.halves.take()
            {
                stack.push(l);
                stack.push(r);
            }
        }
    }
}

impl core::ops::Deref for SmallStr {
    type Target = str;

    #[inline]
    fn deref(&self) -> &str {
        self.as_str()
    }
}

impl core::borrow::Borrow<str> for SmallStr {
    fn borrow(&self) -> &str {
        self.as_str()
    }
}

impl From<&str> for SmallStr {
    fn from(s: &str) -> Self {
        match u8::try_from(s.len()) {
            Ok(len) if s.len() <= INLINE => {
                let mut bytes = [0; INLINE];
                bytes[..s.len()].copy_from_slice(s.as_bytes());
                Self(Repr::Inline { len, bytes })
            }
            _ => Self(Repr::Heap(Arc::from(s))),
        }
    }
}

pub struct Pieces {
    to: *mut u8,
    len: usize,
    at: usize,
}

impl Pieces {
    pub fn push(&mut self, piece: &str) {
        assert!(piece.len() <= self.len - self.at, "a piece runs past the announced length");
        unsafe { core::ptr::copy_nonoverlapping(piece.as_ptr(), self.to.add(self.at), piece.len()) };
        self.at += piece.len();
    }

    pub fn push_swapped(&mut self, piece: &str, from: u8, to: u8) {
        assert!(from.is_ascii() && to.is_ascii(), "only ASCII bytes are swapped, which keeps UTF-8 well formed");
        assert!(piece.len() <= self.len - self.at, "a piece runs past the announced length");
        let out = unsafe { core::slice::from_raw_parts_mut(self.to.add(self.at).cast::<core::mem::MaybeUninit<u8>>(), piece.len()) };
        for (o, &b) in out.iter_mut().zip(piece.as_bytes()) {
            o.write(if b == from { to } else { b });
        }
        self.at += piece.len();
    }
}

impl SmallStr {
    pub fn build(len: usize, fill: impl FnOnce(&mut Pieces)) -> Self {
        if let Ok(n) = u8::try_from(len)
            && len <= INLINE
        {
            let mut bytes = [0; INLINE];
            let mut pieces = Pieces { to: bytes.as_mut_ptr(), len, at: 0 };
            fill(&mut pieces);
            assert!(pieces.at == len, "the pieces add up to the announced length");
            return Self(Repr::Inline { len: n, bytes });
        }
        let mut buf = Arc::<[u8]>::new_uninit_slice(len);
        let to = Arc::get_mut(&mut buf).expect("a fresh buffer").as_mut_ptr().cast::<u8>();
        let mut pieces = Pieces { to, len, at: 0 };
        fill(&mut pieces);
        assert!(pieces.at == len, "the pieces add up to the announced length");
        let raw = Arc::into_raw(unsafe { buf.assume_init() });
        Self(Repr::Heap(unsafe { Arc::from_raw(raw as *const str) }))
    }
}

impl fmt::Debug for SmallStr {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        fmt::Debug::fmt(self.as_str(), f)
    }
}

impl PartialEq for SmallStr {
    #[inline]
    fn eq(&self, o: &Self) -> bool {
        self.byte_len() == o.byte_len() && self.as_str() == o.as_str()
    }
}

impl Eq for SmallStr {}

impl PartialOrd for SmallStr {
    fn partial_cmp(&self, o: &Self) -> Option<Ordering> {
        Some(self.cmp(o))
    }
}

impl Ord for SmallStr {
    fn cmp(&self, o: &Self) -> Ordering {
        self.as_str().cmp(o.as_str())
    }
}

impl Hash for SmallStr {
    fn hash<H: Hasher>(&self, h: &mut H) {
        self.as_str().hash(h);
    }
}

#[derive(Collect)]
#[collect(require_static)]
pub struct JsStr {
    text: SmallStr,
    utf16: AtomicPtr<Wide>,
}

struct Wide(alloc::boxed::Box<[u16]>);

const _: () = assert!(core::mem::size_of::<JsStr>() == 32, "a js string stays two words plus the utf16 state");

impl Drop for JsStr {
    fn drop(&mut self) {
        let p = *self.utf16.get_mut();
        if !p.is_null() && !core::ptr::eq(p, Self::ASCII) {
            drop(unsafe { alloc::boxed::Box::from_raw(p) });
        }
    }
}

impl JsStr {
    const ASCII: *mut Wide = core::ptr::without_provenance_mut(1);

    pub fn text(&self) -> &SmallStr {
        &self.text
    }

    #[inline]
    pub fn unit_ascii(&self) -> bool {
        let p = self.utf16.load(AtomicOrdering::Acquire);
        if core::ptr::eq(p, Self::ASCII) {
            return true;
        }
        if !p.is_null() {
            return false;
        }
        if !self.text.is_ascii() {
            return false;
        }
        match self.utf16.compare_exchange(core::ptr::null_mut(), Self::ASCII, AtomicOrdering::AcqRel, AtomicOrdering::Acquire) {
            Ok(_) => true,
            Err(won) => core::ptr::eq(won, Self::ASCII),
        }
    }

    fn ensure(&self) -> *mut Wide {
        let p = self.utf16.load(AtomicOrdering::Acquire);
        if !p.is_null() {
            return p;
        }
        if self.unit_ascii() {
            return Self::ASCII;
        }
        let wide = alloc::boxed::Box::into_raw(alloc::boxed::Box::new(Wide(self.text.encode_utf16().collect())));
        match self.utf16.compare_exchange(core::ptr::null_mut(), wide, AtomicOrdering::AcqRel, AtomicOrdering::Acquire) {
            Ok(_) => wide,
            Err(won) => {
                drop(unsafe { alloc::boxed::Box::from_raw(wide) });
                won
            }
        }
    }

    unsafe fn wide_units<'a>(p: *const Wide) -> &'a [u16] {
        unsafe { &(*p).0 }
    }

    pub fn utf16_len(&self) -> usize {
        let p = self.ensure();
        if core::ptr::eq(p, Self::ASCII) { self.text.byte_len() } else { unsafe { Self::wide_units(p).len() } }
    }

    pub fn code_unit(&self, i: usize) -> Option<u16> {
        let p = self.ensure();
        if core::ptr::eq(p, Self::ASCII) { self.text.as_bytes().get(i).copied().map(u16::from) } else { unsafe { Self::wide_units(p).get(i).copied() } }
    }

    pub fn wide(&self) -> Option<&[u16]> {
        let p = self.ensure();
        if core::ptr::eq(p, Self::ASCII) { None } else { Some(unsafe { Self::wide_units(p) }) }
    }

    pub fn from_ascii(text: SmallStr) -> Self {
        Self { text, utf16: AtomicPtr::new(Self::ASCII) }
    }
}

impl From<SmallStr> for JsStr {
    fn from(text: SmallStr) -> Self {
        Self { text, utf16: AtomicPtr::new(core::ptr::null_mut()) }
    }
}

impl From<&str> for JsStr {
    fn from(s: &str) -> Self {
        Self::from(SmallStr::from(s))
    }
}

impl core::ops::Deref for JsStr {
    type Target = str;

    fn deref(&self) -> &str {
        self.text.as_str()
    }
}

impl core::borrow::Borrow<str> for JsStr {
    fn borrow(&self) -> &str {
        self.text.as_str()
    }
}

impl fmt::Debug for JsStr {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        fmt::Debug::fmt(&self.text, f)
    }
}

impl PartialEq for JsStr {
    fn eq(&self, o: &Self) -> bool {
        self.text == o.text
    }
}

impl Eq for JsStr {}

impl PartialOrd for JsStr {
    fn partial_cmp(&self, o: &Self) -> Option<Ordering> {
        Some(self.cmp(o))
    }
}

impl Ord for JsStr {
    fn cmp(&self, o: &Self) -> Ordering {
        self.text.cmp(&o.text)
    }
}

impl Hash for JsStr {
    fn hash<H: Hasher>(&self, h: &mut H) {
        self.text.hash(h);
    }
}

#[derive(Clone, Copy)]
pub enum Units<'a> {
    Ascii(&'a [u8]),
    Wide(&'a [u16]),
}

impl Units<'_> {
    pub fn len(self) -> usize {
        match self {
            Units::Ascii(b) => b.len(),
            Units::Wide(w) => w.len(),
        }
    }

    pub fn is_empty(self) -> bool {
        self.len() == 0
    }

    pub fn get(self, i: usize) -> Option<u16> {
        match self {
            Units::Ascii(b) => b.get(i).map(|&c| u16::from(c)),
            Units::Wide(w) => w.get(i).copied(),
        }
    }

    pub fn substring(self, r: core::ops::Range<usize>) -> String {
        let end = r.end.min(self.len());
        let start = r.start.min(end);
        match self {
            Units::Ascii(b) => b[start..end].iter().map(|&c| char::from(c)).collect(),
            Units::Wide(w) => String::from_utf16_lossy(&w[start..end]),
        }
    }

    pub fn to_vec(self) -> Vec<u16> {
        match self {
            Units::Ascii(b) => b.iter().map(|&c| u16::from(c)).collect(),
            Units::Wide(w) => w.to_vec(),
        }
    }
}

impl JsStr {
    pub fn units(&self) -> Units<'_> {
        match self.wide() {
            Some(w) => Units::Wide(w),
            None => Units::Ascii(self.text.as_bytes()),
        }
    }
}
