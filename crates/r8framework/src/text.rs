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
