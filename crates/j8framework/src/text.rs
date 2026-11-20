//! Strings (shared by property keys and JS string values).
//!
//! Short ones (at most [`INLINE`] bytes) are stored inline without a separate allocation; long ones share a single heap
//! copy, and a piece cut out of a long one (`split`, `slice`, a regex match) points into that copy instead of copying
//! (V8's sliced strings). When string `+` produces something long enough it is first stored as a rope (two halves plus
//! the total length) and flattened and cached only when the content is first needed (so a `s += c` loop no longer
//! copies the whole string each time). Flattening and dropping are both iterative, so ropes hundreds of thousands of
//! levels deep don't overflow the stack. No `Gc`: the GC doesn't need to manage it.
use alloc::sync::Arc;
use alloc::string::String;
use alloc::vec::Vec;
use core::cmp::Ordering;
use core::fmt;
use core::hash::{Hash, Hasher};

use j8lockfree::OnceBox;

use crate::{Collect};

/// Below this many bytes, concatenation flattens directly (copying a short string is cheaper than building a rope node)
const ROPE_MIN: usize = 128;
/// The longest string stored inline
const INLINE: usize = 22;
/// The longest piece kept as a slice of its parent's copy (longer pieces get their own: the length is 16 bits)
const SLICE_MAX: usize = u16::MAX as usize;

/// A string.
#[derive(Clone, Collect)]
#[collect(require_static)]
pub struct SmallStr(Repr);

#[derive(Clone)]
enum Repr {
    Inline { len: u8, bytes: [u8; INLINE] },
    Heap(Arc<str>),
    /// Bytes `start .. start + len` of `buf` (on character boundaries)
    Slice { len: u16, start: u32, buf: Arc<str> },
    Rope(Arc<Rope>),
}

const _: () = assert!(core::mem::size_of::<SmallStr>() == 24, "a string is three words");

/// A rope node: two halves plus the flattened text once someone needed it. The halves stay until the node is dropped
/// (other threads may be reading them while one flattens; freeing them early would need hazard pointers).
struct Rope {
    len: usize,
    flat: OnceBox<SmallStr>,
    halves: Option<(SmallStr, SmallStr)>,
}

impl SmallStr {
    /// `a + b`
    pub fn concat(a: &SmallStr, b: &SmallStr) -> SmallStr {
        let (la, lb) = (a.byte_len(), b.byte_len());
        if lb == 0 {
            return a.clone();
        }
        if la == 0 {
            return b.clone();
        }
        if la + lb < ROPE_MIN {
            let mut s = String::with_capacity(la + lb);
            s.push_str(a);
            s.push_str(b);
            return Self::from(s.as_str());
        }
        Self(Repr::Rope(Arc::new(Rope { len: la + lb, flat: OnceBox::default(), halves: Some((a.clone(), b.clone())) })))
    }

    /// Byte count (without flattening)
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
            // SAFETY: the inline bytes are a copy of a `str` (cut on a character boundary)
            Repr::Inline { len, bytes } => unsafe { core::str::from_utf8_unchecked(&bytes[..usize::from(*len)]) },
            Repr::Heap(s) => s,
            // SAFETY: `slice` cut the range on character boundaries inside `buf`
            Repr::Slice { len, start, buf } => unsafe { buf.get_unchecked(*start as usize..*start as usize + usize::from(*len)) },
            Repr::Rope(r) => r.flat.get_or_init(|| r.flatten()).as_str(),
        }
    }

    /// `piece`, borrowed from this string's text, as a string of its own: short pieces are copied inline, longer
    /// ones point into this string's heap copy; `None` for text from anywhere else
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
    /// Concatenate the leaves in order (nested nodes that are already flat contribute their text)
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
    /// Iteratively release child nodes held only by us (recursively dropping a deep rope overflows the stack)
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

impl fmt::Debug for SmallStr {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        fmt::Debug::fmt(self.as_str(), f)
    }
}

/// Comparison and hashing are by content (consistent with `str`, as `Borrow<str>` lookups require)
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

/// A JS string value: content plus a lazily computed UTF-16 view.
///
/// Content is stored as UTF-8, while JS lengths and indices count UTF-16 code units: on first use it is scanned once.
/// For pure ASCII the code units are the bytes (only a flag is recorded); otherwise the code unit array is computed and
/// kept. After that `length`, `s[i]`, and `charCodeAt` are all O(1).
#[derive(Collect)]
#[collect(require_static)]
pub struct JsStr {
    text: SmallStr,
    utf16: OnceBox<Utf16>,
}

/// The UTF-16 view.
enum Utf16 {
    /// Pure ASCII: the i-th code unit is the i-th byte
    Ascii,
    Wide(alloc::boxed::Box<[u16]>),
}

impl JsStr {
    pub fn text(&self) -> &SmallStr {
        &self.text
    }

    fn utf16(&self) -> &Utf16 {
        self.utf16.get_or_init(|| if self.text.is_ascii() { Utf16::Ascii } else { Utf16::Wide(self.text.encode_utf16().collect()) })
    }

    /// UTF-16 code unit count (JS `length`)
    pub fn utf16_len(&self) -> usize {
        match self.utf16() {
            Utf16::Ascii => self.text.byte_len(),
            Utf16::Wide(w) => w.len(),
        }
    }

    /// The `i`-th UTF-16 code unit
    pub fn code_unit(&self, i: usize) -> Option<u16> {
        match self.utf16() {
            Utf16::Ascii => self.text.as_bytes().get(i).map(|&b| u16::from(b)),
            Utf16::Wide(w) => w.get(i).copied(),
        }
    }

    /// All UTF-16 code units; `None` for pure ASCII (the code units are the bytes; the caller uses `as_bytes` directly)
    pub fn wide(&self) -> Option<&[u16]> {
        match self.utf16() {
            Utf16::Ascii => None,
            Utf16::Wide(w) => Some(w),
        }
    }
}

impl From<SmallStr> for JsStr {
    fn from(text: SmallStr) -> Self {
        Self { text, utf16: OnceBox::default() }
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

/// A (borrowed) UTF-16 code unit view of a string: pure ASCII uses the bytes directly.
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

    /// The substring `[start, end)` (out-of-range parts are cut off; pure ASCII is sliced directly without transcoding)
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
    /// The UTF-16 code unit view (scanned once on first use to see whether it is pure ASCII; O(1) afterwards)
    pub fn units(&self) -> Units<'_> {
        match self.utf16() {
            Utf16::Ascii => Units::Ascii(self.text.as_bytes()),
            Utf16::Wide(w) => Units::Wide(w),
        }
    }
}
