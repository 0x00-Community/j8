use alloc::sync::Arc;
use alloc::string::String;
use core::cmp::Ordering;
use core::fmt;
use core::hash::{Hash, Hasher};
use core::mem::MaybeUninit;
use core::sync::atomic::{AtomicU32, Ordering as AtomicOrdering};

use crate::{Collect, Gc, Mutation};

const INLINE: usize = 22;

#[derive(Clone, Collect)]
#[collect(require_static)]
pub struct SmallStr(Repr);

#[derive(Clone)]
enum Repr {
    Inline { len: u8, bytes: [u8; INLINE] },
    Heap(Arc<str>),
}

const _: () = assert!(core::mem::size_of::<SmallStr>() == 24, "a string is three words");

impl SmallStr {
    #[inline]
    pub fn byte_len(&self) -> usize {
        match &self.0 {
            Repr::Inline { len, .. } => usize::from(*len),
            Repr::Heap(s) => s.len(),
        }
    }

    #[inline]
    pub fn as_str(&self) -> &str {
        match &self.0 {
            Repr::Inline { len, bytes } => unsafe { core::str::from_utf8_unchecked(&bytes[..usize::from(*len)]) },
            Repr::Heap(s) => s,
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

const EXTEND_MIN: usize = 128;

const VIEW_MIN: usize = 32;

#[repr(C)]
#[derive(Collect)]
#[cfg_attr(all(target_os = "linux", feature = "crossheap"), derive(paragc::Shared))]
pub struct JsStr<'gc> {
    buf: Option<Gc<'gc, JsStr<'gc>>>,
    len: u32,
    units: u32,
    start: u32,
    cap: u32,
    used: AtomicU32,
}

const _: () = assert!(core::mem::size_of::<JsStr<'static>>() == 32, "a js string is four words before its bytes");
const _: () = assert!(core::mem::offset_of!(JsStr<'static>, units) == 12);
const _: () = assert!(JsStr::UNITS_OFFSET == 12);

type Tail<'a> = &'a mut [MaybeUninit<u8>];

impl<'gc> JsStr<'gc> {
    pub const UNITS_OFFSET: usize = core::mem::offset_of!(JsStr<'static>, units);

    const LONE: u32 = 1 << 31;

    fn capacity(&self) -> usize {
        (self.cap & !Self::LONE) as usize
    }

    #[inline]
    pub fn lone_surrogates(&self) -> bool {
        self.buf.is_none() && self.cap & Self::LONE != 0
    }

    fn tail(&self) -> *const u8 {
        (&raw const *self).wrapping_add(1).cast()
    }

    fn backing(this: Gc<'gc, Self>) -> (Gc<'gc, Self>, usize) {
        match this.buf {
            Some(b) => (b, this.start as usize),
            None => (this, 0),
        }
    }

    #[inline]
    pub fn byte_len(&self) -> usize {
        self.len as usize
    }

    #[inline]
    pub fn as_str(&self) -> &str {
        let (tail, start) = match &self.buf {
            Some(b) => (Gc::get(*b).tail(), self.start as usize),
            None => (self.tail(), 0),
        };
        unsafe { core::str::from_utf8_unchecked(core::slice::from_raw_parts(tail.add(start), self.byte_len())) }
    }

    #[inline]
    pub fn unit_ascii(&self) -> bool {
        self.len == self.units
    }

    pub fn utf16_len(&self) -> usize {
        self.units as usize
    }

    pub fn code_unit(&self, i: usize) -> Option<u16> {
        self.units().get(i)
    }

    #[expect(clippy::cast_ptr_alignment)]
    pub fn wide(&self) -> Option<&[u16]> {
        (!self.unit_ascii()).then(|| unsafe { core::slice::from_raw_parts(self.tail().add(self.capacity().next_multiple_of(2)).cast::<u16>(), self.utf16_len()) })
    }

    pub fn source_text(&self) -> String {
        if self.lone_surrogates() {
            HiddenSurrogates::hide(self.encode_utf16())
        } else {
            String::from(self.as_str())
        }
    }

    pub fn encode_utf16(&self) -> impl Iterator<Item = u16> + Clone + '_ {
        self.units().iter()
    }

    pub fn units(&self) -> Units<'_> {
        match self.wide() {
            Some(w) => Units::Wide(w),
            None => Units::Ascii(self.as_bytes()),
        }
    }

    fn place(mc: &Mutation<'gc>, s: Self, tail: usize, fill: impl FnOnce(Tail<'_>)) -> Gc<'gc, Self> {
        #[cfg(all(target_os = "linux", feature = "crossheap"))]
        return mc.mutator().alloc_shared_tail(s, tail, fill);
        #[cfg(not(all(target_os = "linux", feature = "crossheap")))]
        mc.mutator().alloc_tail(s, tail, fill)
    }

    fn own(mc: &Mutation<'gc>, len: usize, units: usize, cap: usize, lone: bool, fill: impl FnOnce(Tail<'_>, Tail<'_>)) -> Gc<'gc, Self> {
        let n = |x: usize| u32::try_from(x).ok().filter(|x| x & Self::LONE == 0).expect("a string is shorter than 2 GiB");
        let wide = if len == units && !lone { 0 } else { 2 * units };
        let head = if wide == 0 { cap } else { cap.next_multiple_of(2) };
        let mark = if lone { Self::LONE } else { 0 };
        let s = Self { buf: None, len: n(len), units: n(units), start: 0, cap: n(cap) | mark, used: AtomicU32::new(n(len)) };
        Self::place(mc, s, head + wide, |t| {
            let (bytes, rest) = t.split_at_mut(head);
            fill(&mut bytes[..len], &mut rest[..wide]);
        })
    }

    fn write(to: Tail<'_>, from: &[u8]) -> usize {
        for (t, &b) in to.iter_mut().zip(from) {
            t.write(b);
        }
        from.len()
    }

    fn write_units(to: Tail<'_>, from: impl Iterator<Item = u16>) -> usize {
        let mut n = 0;
        for (t, u) in to.as_chunks_mut::<2>().0.iter_mut().zip(from) {
            let [lo, hi] = u.to_ne_bytes();
            t[0].write(lo);
            t[1].write(hi);
            n += 2;
        }
        n
    }

    pub fn alloc(mc: &Mutation<'gc>, s: &str) -> Gc<'gc, Self> {
        Self::alloc_known(mc, s, s.is_ascii())
    }

    pub fn alloc_known(mc: &Mutation<'gc>, s: &str, ascii: bool) -> Gc<'gc, Self> {
        let units = if ascii { s.len() } else { s.encode_utf16().count() };
        Self::own(mc, s.len(), units, s.len(), false, |bytes, wide| {
            Self::write(bytes, s.as_bytes());
            Self::write_units(wide, s.encode_utf16());
        })
    }

    pub fn alloc_units(mc: &Mutation<'gc>, units: &[u16]) -> Gc<'gc, Self> {
        if let Ok(s) = String::from_utf16(units) {
            return Self::alloc(mc, &s);
        }
        let lossy = String::from_utf16_lossy(units);
        Self::own(mc, lossy.len(), units.len(), lossy.len(), true, |bytes, wide| {
            Self::write(bytes, lossy.as_bytes());
            Self::write_units(wide, units.iter().copied());
        })
    }

    pub fn slice_units(mc: &Mutation<'gc>, of: Gc<'gc, Self>, r: core::ops::Range<usize>) -> Gc<'gc, Self> {
        if r.start == 0 && r.end == of.utf16_len() {
            return of;
        }
        match of.units() {
            Units::Ascii(b) => Self::alloc_known(mc, core::str::from_utf8(&b[r]).unwrap_or_default(), true),
            Units::Wide(w) => Self::alloc_units(mc, &w[r]),
        }
    }

    pub fn concat(mc: &Mutation<'gc>, a: Gc<'gc, Self>, b: Gc<'gc, Self>) -> Gc<'gc, Self> {
        if b.len == 0 {
            return a;
        }
        if a.len == 0 {
            return b;
        }
        if a.lone_surrogates() || b.lone_surrogates() {
            let units: alloc::vec::Vec<u16> = a.encode_utf16().chain(b.encode_utf16()).collect();
            return Self::alloc_units(mc, &units);
        }
        let total = a.byte_len() + b.byte_len();
        let ascii = a.unit_ascii() && b.unit_ascii();
        if ascii
            && total >= EXTEND_MIN
            && let Some(s) = Self::extend(mc, a, b)
        {
            return s;
        }
        let cap = if ascii && total >= EXTEND_MIN { total.saturating_mul(2).min((Self::LONE - 1) as usize) } else { total };
        Self::own(mc, total, a.utf16_len() + b.utf16_len(), cap, false, |bytes, wide| {
            let at = Self::write(bytes, a.as_bytes());
            Self::write(&mut bytes[at..], b.as_bytes());
            let at = Self::write_units(wide, a.units().iter());
            Self::write_units(&mut wide[at..], b.units().iter());
        })
    }

    pub fn concat_ascii(mc: &Mutation<'gc>, s: Gc<'gc, Self>, extra: &[u8], extra_first: bool) -> Gc<'gc, Self> {
        debug_assert!(extra.is_ascii(), "appended text is ASCII");
        if extra.is_empty() {
            return s;
        }
        if s.len == 0 {
            return Self::alloc_known(mc, core::str::from_utf8(extra).expect("appended text is ASCII"), true);
        }
        if s.lone_surrogates() || !s.unit_ascii() {
            return Self::concat_wide(mc, s, extra, extra_first);
        }
        let total = s.byte_len() + extra.len();
        if !extra_first
            && total >= EXTEND_MIN
            && let Some(v) = Self::extend_bytes(mc, s, extra)
        {
            return v;
        }
        let cap = if !extra_first && total >= EXTEND_MIN { total.saturating_mul(2).min((Self::LONE - 1) as usize) } else { total };
        Self::own(mc, total, total, cap, false, |bytes, _| {
            let (first, second) = if extra_first { (extra, s.as_bytes()) } else { (s.as_bytes(), extra) };
            let at = Self::write(bytes, first);
            Self::write(&mut bytes[at..], second);
        })
    }

    #[cold]
    #[inline(never)]
    fn concat_wide(mc: &Mutation<'gc>, s: Gc<'gc, Self>, extra: &[u8], extra_first: bool) -> Gc<'gc, Self> {
        if s.lone_surrogates() {
            let mut units = alloc::vec::Vec::with_capacity(s.utf16_len() + extra.len());
            if extra_first {
                units.extend(extra.iter().copied().map(u16::from));
                units.extend(s.encode_utf16());
            } else {
                units.extend(s.encode_utf16());
                units.extend(extra.iter().copied().map(u16::from));
            }
            return Self::alloc_units(mc, &units);
        }
        let total = s.byte_len() + extra.len();
        let units = s.utf16_len() + extra.len();
        Self::own(mc, total, units, total, false, |bytes, wide| {
            if extra_first {
                let at = Self::write(bytes, extra);
                Self::write(&mut bytes[at..], s.as_bytes());
                let at = Self::write_units(wide, extra.iter().copied().map(u16::from));
                Self::write_units(&mut wide[at..], s.units().iter());
            } else {
                let at = Self::write(bytes, s.as_bytes());
                Self::write(&mut bytes[at..], extra);
                let at = Self::write_units(wide, s.units().iter());
                Self::write_units(&mut wide[at..], extra.iter().copied().map(u16::from));
            }
        })
    }

    fn extend(mc: &Mutation<'gc>, a: Gc<'gc, Self>, b: Gc<'gc, Self>) -> Option<Gc<'gc, Self>> {
        Self::extend_bytes(mc, a, b.as_str().as_bytes())
    }

    fn extend_bytes(mc: &Mutation<'gc>, a: Gc<'gc, Self>, bytes: &[u8]) -> Option<Gc<'gc, Self>> {
        let (own, start) = Self::backing(a);
        let end = start + a.byte_len();
        if own.capacity().checked_sub(end)? < bytes.len() {
            return None;
        }
        let (from, to) = (u32::try_from(end).ok()?, u32::try_from(end + bytes.len()).ok()?);
        own.used.compare_exchange(from, to, AtomicOrdering::AcqRel, AtomicOrdering::Relaxed).ok()?;
        unsafe { core::ptr::copy_nonoverlapping(bytes.as_ptr(), own.as_ptr().cast_mut().add(1).cast::<u8>().add(end), bytes.len()) };
        Some(Self::view(mc, own, start, a.byte_len() + bytes.len()))
    }

    fn view(mc: &Mutation<'gc>, own: Gc<'gc, Self>, start: usize, len: usize) -> Gc<'gc, Self> {
        let n = |x: usize| u32::try_from(x).expect("a string is shorter than 4 GiB");
        Self::place(mc, Self { buf: Some(own), len: n(len), units: n(len), start: n(start), cap: 0, used: AtomicU32::new(0) }, 0, |_| {})
    }

    pub fn piece(mc: &Mutation<'gc>, of: Gc<'gc, Self>, piece: &str) -> Gc<'gc, Self> {
        let text = of.as_str();
        let at = piece.as_ptr().addr().checked_sub(text.as_ptr().addr()).filter(|&at| at + piece.len() <= text.len());
        match at {
            Some(_) if piece.len() == text.len() => of,
            Some(at) if of.unit_ascii() && piece.len() >= VIEW_MIN => {
                let (own, start) = Self::backing(of);
                Self::view(mc, own, start + at, piece.len())
            }
            _ => Self::alloc_known(mc, piece, of.unit_ascii()),
        }
    }
}

impl core::ops::Deref for JsStr<'_> {
    type Target = str;

    fn deref(&self) -> &str {
        self.as_str()
    }
}

impl core::borrow::Borrow<str> for JsStr<'_> {
    fn borrow(&self) -> &str {
        self.as_str()
    }
}

impl fmt::Debug for JsStr<'_> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        fmt::Debug::fmt(self.as_str(), f)
    }
}

impl PartialEq for JsStr<'_> {
    fn eq(&self, o: &Self) -> bool {
        if self.lone_surrogates() || o.lone_surrogates() {
            return self.lone_surrogates() == o.lone_surrogates() && self.wide() == o.wide();
        }
        self.len == o.len && self.as_str() == o.as_str()
    }
}

impl Eq for JsStr<'_> {}

impl PartialOrd for JsStr<'_> {
    fn partial_cmp(&self, o: &Self) -> Option<Ordering> {
        Some(self.cmp(o))
    }
}

impl Ord for JsStr<'_> {
    fn cmp(&self, o: &Self) -> Ordering {
        if self.unit_ascii() && o.unit_ascii() {
            return self.as_str().cmp(o.as_str());
        }
        self.units().iter().cmp(o.units().iter())
    }
}

impl Hash for JsStr<'_> {
    fn hash<H: Hasher>(&self, h: &mut H) {
        match self.wide().filter(|_| self.lone_surrogates()) {
            Some(w) => w.hash(h),
            None => self.as_str().hash(h),
        }
    }
}

pub struct HiddenSurrogates;

impl HiddenSurrogates {
    const BASE: u32 = 0x10_F800;
    const SURROGATES: u32 = 0x800;
    const FIRST: u32 = 0xD800;

    fn hidden(c: char) -> Option<u16> {
        let v = u32::from(c).checked_sub(Self::BASE).filter(|&v| v < Self::SURROGATES)?;
        u16::try_from(v + Self::FIRST).ok()
    }

    pub fn hide(units: impl Iterator<Item = u16>) -> String {
        char::decode_utf16(units).map(|c| c.unwrap_or_else(|e| char::from_u32(Self::BASE + u32::from(e.unpaired_surrogate()) - Self::FIRST).unwrap_or(char::REPLACEMENT_CHARACTER))).collect()
    }

    pub fn reveal(text: &str) -> Option<alloc::vec::Vec<u16>> {
        if !text.chars().any(|c| Self::hidden(c).is_some()) {
            return None;
        }
        let mut out = alloc::vec::Vec::with_capacity(text.len());
        for c in text.chars() {
            match Self::hidden(c) {
                Some(u) => out.push(u),
                None => out.extend_from_slice(c.encode_utf16(&mut [0; 2])),
            }
        }
        Some(out)
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

    pub fn to_vec(self) -> Vec<u16> {
        match self {
            Units::Ascii(b) => b.iter().map(|&c| u16::from(c)).collect(),
            Units::Wide(w) => w.to_vec(),
        }
    }
}

impl<'a> Units<'a> {
    pub fn iter(self) -> impl Iterator<Item = u16> + Clone + 'a {
        let (ascii, wide) = match self {
            Units::Ascii(b) => (b, &[][..]),
            Units::Wide(w) => (&[][..], w),
        };
        ascii.iter().map(|&c| u16::from(c)).chain(wide.iter().copied())
    }
}

#[cfg(test)]
mod tests {
    use core::num::NonZeroUsize;

    use super::JsStr;
    use crate::{Heap, Mutation};

    fn with_heap(f: impl FnOnce(&Mutation<'_>)) {
        let heap = Heap::new(NonZeroUsize::MIN);
        heap.mutate(|m| {
            let mc = unsafe { Mutation::new(m, 0) };
            f(&mc);
        });
    }

    fn ascii<'gc>(mc: &Mutation<'gc>, n: usize, byte: u8) -> crate::Gc<'gc, JsStr<'gc>> {
        JsStr::alloc_known(mc, core::str::from_utf8(&alloc::vec![byte; n]).expect("one ASCII byte"), true)
    }

    #[test]
    fn concat_ascii_matches_concat_including_lone_surrogates() {
        with_heap(|mc| {
            let left = JsStr::alloc(mc, "é");
            let digits = JsStr::alloc(mc, "21");
            let joined = JsStr::concat_ascii(mc, left, b"21", false);
            assert_eq!(&*joined, &*JsStr::concat(mc, left, digits), "ASCII digits after a non-ASCII string");
            let before = JsStr::concat_ascii(mc, left, b"21", true);
            assert_eq!(&*before, &*JsStr::concat(mc, digits, left), "ASCII digits before a non-ASCII string");
            let high = JsStr::alloc_units(mc, &[0xD800]);
            let low = JsStr::alloc_units(mc, &[0xDE00]);
            let pair = JsStr::concat(mc, high, low);
            assert!(!pair.lone_surrogates(), "a split surrogate pair is one code point");
            assert_eq!(pair.utf16_len(), 2, "the pair is two units");
            assert_eq!((pair.code_unit(0), pair.code_unit(1)), (Some(0xD800), Some(0xDE00)), "the units stay the surrogate pair");
            assert_eq!(pair.as_str().chars().count(), 1, "the pair is one scalar");
            let held = JsStr::concat_ascii(mc, high, b"A", false);
            assert_eq!(&*held, &*JsStr::concat(mc, high, JsStr::alloc(mc, "A")), "a digit cannot complete a lone surrogate");
            assert!(held.lone_surrogates(), "the high surrogate is still lone");
            let grown = JsStr::concat(mc, JsStr::concat(mc, JsStr::alloc(mc, "ab"), high), low);
            assert!(!grown.lone_surrogates(), "a high surrogate at the end of one piece joins the next low surrogate");
            assert_eq!(grown.as_str(), "ab\u{10200}", "D800 DE00 is U+10200");
        });
    }

    #[test]
    fn long_appends_keep_spare_and_prepends_stay_exact() {
        with_heap(|mc| {
            let short = JsStr::concat(mc, JsStr::alloc(mc, "hi"), JsStr::alloc(mc, "!"));
            assert_eq!(short.capacity(), short.byte_len(), "a short result is exact");
            let fresh = ascii(mc, 200, b'x');
            let doubled = JsStr::concat(mc, fresh, JsStr::alloc(mc, "b"));
            assert_eq!(doubled.capacity(), doubled.byte_len() * 2, "a long ASCII append keeps spare");
            assert_eq!(doubled.as_str(), &("x".repeat(200) + "b"), "the doubled result is the concatenation");
            let head = ascii(mc, 100, b'a');
            let tail = ascii(mc, 60, b'b');
            let owned = JsStr::concat(mc, head, tail);
            assert!(owned.capacity() >= owned.byte_len() * 2, "the first long join has spare");
            let extended = JsStr::concat(mc, owned, JsStr::alloc(mc, "c"));
            assert_eq!(extended.as_str(), &("a".repeat(100) + &"b".repeat(60) + "c"), "the spare is claimed by the next append");
            let forked = JsStr::concat(mc, owned, JsStr::alloc(mc, "!"));
            assert!(forked.capacity() >= forked.byte_len() * 2, "a forked append still keeps spare for whoever holds the result");
            assert_eq!(forked.as_str(), &("a".repeat(100) + &"b".repeat(60) + "!"), "the fork is the prefix plus the new piece");
            assert_eq!(owned.as_str(), &("a".repeat(100) + &"b".repeat(60)), "the prefix is unchanged");
            let same = ascii(mc, 80, b'q');
            let doubled_self = JsStr::concat(mc, same, same);
            assert_eq!(doubled_self.as_str(), "q".repeat(160), "appending a string to itself copies both halves");
            let prefix = ascii(mc, 180, b'm');
            let one = JsStr::concat(mc, prefix, JsStr::alloc(mc, "a"));
            let other = JsStr::concat(mc, prefix, JsStr::alloc(mc, "b"));
            assert_eq!(one.as_str(), &("m".repeat(180) + "a"), "the first continuation");
            assert_eq!(other.as_str(), &("m".repeat(180) + "b"), "the second continuation of the same prefix");
            let pre = JsStr::concat_ascii(mc, fresh, b"n", true);
            assert_eq!(pre.capacity(), pre.byte_len(), "a prepend has no spare an append could not claim");
            assert_eq!(pre.as_str(), &("n".to_owned() + &"x".repeat(200)), "the digits come first");
        });
    }
}
