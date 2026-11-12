//! 字符串（属性键与 JS 字符串值共用）。
//!
//! 短的（不超过 23 字节）内联存、不另外分配，长的共享一份堆上副本；字符串 `+` 拼出来够长时先存成绳索
//! （两半加总长），第一次要内容时才拼平并缓存（`s += c` 循环不再每次复制整串）。拼平与析构都是迭代的，
//! 几十万层深的绳索也不会爆栈。没有 `Gc`：GC 不用管它。
use alloc::rc::Rc;
use alloc::string::String;
use alloc::vec::Vec;
use core::cell::{Cell, OnceCell};
use core::cmp::Ordering;
use core::fmt;
use core::hash::{Hash, Hasher};

use r8gc::Collect;
use smol_str::SmolStr;

/// 拼起来不到这么多字节就直接拼平（短串复制比建绳索节点便宜）
const ROPE_MIN: usize = 128;

/// 一个字符串。
#[derive(Clone, Collect)]
#[collect(require_static)]
pub struct SmallStr(Repr);

#[derive(Clone)]
enum Repr {
    Flat(SmolStr),
    Rope(Rc<Rope>),
}

/// 绳索节点：拼平之前存两半，拼平之后两半放掉、只留结果。
struct Rope {
    len: usize,
    flat: OnceCell<SmolStr>,
    halves: Cell<Option<(SmallStr, SmallStr)>>,
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
        Self(Repr::Rope(Rc::new(Rope { len: la + lb, flat: OnceCell::new(), halves: Cell::new(Some((a.clone(), b.clone()))) })))
    }

    /// 字节数（不拼平）
    pub fn byte_len(&self) -> usize {
        match &self.0 {
            Repr::Flat(s) => s.len(),
            Repr::Rope(r) => r.len,
        }
    }

    pub fn as_str(&self) -> &str {
        match &self.0 {
            Repr::Flat(s) => s,
            Repr::Rope(r) => r.flat.get_or_init(|| r.flatten()),
        }
    }
}

impl Rope {
    /// 按顺序把叶子拼起来：自己的两半拿走（拼平后不要了）；嵌套的节点可能被别的串共享，借出来再放回
    fn flatten(&self) -> SmolStr {
        let mut out = String::with_capacity(self.len);
        let (l, r) = self.halves.take().expect("an unflattened rope keeps its halves");
        let mut stack = alloc::vec![r, l];
        while let Some(s) = stack.pop() {
            match &s.0 {
                Repr::Flat(f) => out.push_str(f),
                Repr::Rope(n) => match n.flat.get() {
                    Some(f) => out.push_str(f),
                    None => {
                        let halves = n.halves.take();
                        let (a, b) = halves.clone().expect("an unflattened rope keeps its halves");
                        n.halves.set(halves);
                        stack.push(b);
                        stack.push(a);
                    }
                },
            }
        }
        SmolStr::from(out)
    }
}

impl Drop for Rope {
    /// 迭代地放掉只被自己持有的子节点（递归析构深绳索会爆栈）
    fn drop(&mut self) {
        let mut stack: Vec<SmallStr> = Vec::new();
        if let Some((l, r)) = self.halves.take() {
            stack.push(l);
            stack.push(r);
        }
        while let Some(s) = stack.pop() {
            if let Repr::Rope(rc) = s.0
                && let Ok(node) = Rc::try_unwrap(rc)
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
        Self(Repr::Flat(SmolStr::new(s)))
    }
}

impl fmt::Debug for SmallStr {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        fmt::Debug::fmt(self.as_str(), f)
    }
}

/// 比较、哈希都按内容（与 `str` 一致：`Borrow<str>` 查表要求）
impl PartialEq for SmallStr {
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

/// JS 的字符串值：内容加按需算好的 UTF-16 视图。
///
/// 内容按 UTF-8 存，JS 的长度与下标按 UTF-16 码元算：第一次用到时看一遍——纯 ASCII 的码元就是字节（只记个标记），
/// 否则把码元数组算好留着。之后 `length`、`s[i]`、`charCodeAt` 都是 O(1)。
#[derive(Collect)]
#[collect(require_static)]
pub struct JsStr {
    text: SmallStr,
    utf16: OnceCell<Utf16>,
}

/// UTF-16 视图。
enum Utf16 {
    /// 纯 ASCII：第 i 个码元就是第 i 个字节
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

    /// UTF-16 码元数（JS 的 `length`）
    pub fn utf16_len(&self) -> usize {
        match self.utf16() {
            Utf16::Ascii => self.text.byte_len(),
            Utf16::Wide(w) => w.len(),
        }
    }

    /// 第 `i` 个 UTF-16 码元
    pub fn code_unit(&self, i: usize) -> Option<u16> {
        match self.utf16() {
            Utf16::Ascii => self.text.as_bytes().get(i).map(|&b| u16::from(b)),
            Utf16::Wide(w) => w.get(i).copied(),
        }
    }

    /// 全部 UTF-16 码元；纯 ASCII 给 `None`（码元就是字节，调用方直接用 `as_bytes`）
    pub fn wide(&self) -> Option<&[u16]> {
        match self.utf16() {
            Utf16::Ascii => None,
            Utf16::Wide(w) => Some(w),
        }
    }
}

impl From<SmallStr> for JsStr {
    fn from(text: SmallStr) -> Self {
        Self { text, utf16: OnceCell::new() }
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

/// 字符串的 UTF-16 码元视图（借出来的）：纯 ASCII 直接用字节。
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

    /// `[start, end)` 的子串（越界的部分截掉；纯 ASCII 直接切，不转码）
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
    /// UTF-16 码元视图（第一次用时看一遍是不是纯 ASCII，之后 O(1)）
    pub fn units(&self) -> Units<'_> {
        match self.utf16() {
            Utf16::Ascii => Units::Ascii(self.text.as_bytes()),
            Utf16::Wide(w) => Units::Wide(w),
        }
    }
}
