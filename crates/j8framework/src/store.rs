use alloc::vec::Vec;
use core::sync::atomic::{AtomicU64, Ordering};

use crate::{Collect, Erased, Gc, GcSlot, IsoCell, Mutation, Value, ValueCell, erase};

pub(crate) static INDEXED_STAMP: AtomicU64 = AtomicU64::new(1);

pub(crate) fn bump_indexed() {
    let prev = INDEXED_STAMP.fetch_add(1, Ordering::Relaxed);
    if prev >= (1u64 << 63) - 2 {
        INDEXED_STAMP.store(0, Ordering::Relaxed);
    }
}

pub(crate) fn indexed_stamp() -> u64 {
    INDEXED_STAMP.load(Ordering::Relaxed)
}

pub fn indexed_stamp_addr() -> u64 {
    u64::try_from(core::ptr::from_ref(&INDEXED_STAMP).addr()).expect("the stamp counter's address fits in a machine word")
}

const FAN: usize = 128;

const FAN_BITS: u32 = FAN.trailing_zeros();

const NODE: usize = 1 << 16;

const MIN_LEAF: usize = 2;

#[repr(C)]
#[derive(Collect)]
struct Leaf<'scope, const N: usize> {
    head: usize,
    cells: [ValueCell<'scope>; N],
}

#[repr(C)]
#[derive(Collect)]
struct Node<'scope> {
    head: usize,
    kids: [GcSlot<'scope, Erased>; FAN],
    tail_meta: IsoCell<u64>,
    tail: GcSlot<'scope, Erased>,
}

#[derive(Clone, Copy)]
pub struct Store<'scope>(Gc<'scope, Erased>);

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct StorePath {
    pub head: usize,
    pub kids: Vec<usize>,
    pub cell: usize,
}

impl<'scope> Store<'scope> {
    fn leaf(mc: &Mutation<'scope>, cap: usize, init: &[Value<'scope>]) -> Self {
        fn make<'scope, const N: usize>(mc: &Mutation<'scope>, init: &[Value<'scope>]) -> Gc<'scope, Erased> {
            erase(mc.alloc(Leaf::<N> { head: N, cells: core::array::from_fn(|i| ValueCell::new(init.get(i).copied().unwrap_or_default())) }))
        }
        Self(match cap.max(init.len()).max(MIN_LEAF).next_power_of_two() {
            2 => make::<2>(mc, init),
            4 => make::<4>(mc, init),
            8 => make::<8>(mc, init),
            16 => make::<16>(mc, init),
            32 => make::<32>(mc, init),
            64 => make::<64>(mc, init),
            128 => make::<128>(mc, init),
            n => unreachable!("leaves hold at most {FAN} cells, asked for {n}"),
        })
    }

    fn node(mc: &Mutation<'scope>, level: usize, first: Option<Self>) -> Self {
        let kids = core::array::from_fn(|i| GcSlot::from(if i == 0 { first.map(|s| s.0) } else { None }));
        Self(erase(mc.alloc(Node { head: NODE + level, kids, tail_meta: IsoCell::new(0), tail: GcSlot::default() })))
    }

    fn head(self) -> usize {
        unsafe { *self.0.as_ptr().cast::<usize>() }
    }

    fn is_leaf(self) -> bool {
        self.head() < NODE
    }

    #[inline]
    pub fn capacity(self) -> usize {
        let h = self.head();
        if h < NODE {
            return h;
        }
        let bits = FAN_BITS as usize * (h - NODE + 1);
        if bits >= usize::BITS as usize { usize::MAX } else { 1 << bits }
    }

    fn leaf_cell(self, i: usize) -> &'scope ValueCell<'scope> {
        debug_assert!(self.is_leaf() && i < self.head());
        unsafe { &*self.0.as_ptr().cast::<usize>().add(1).cast::<ValueCell<'scope>>().add(i) }
    }

    fn kid(self, i: usize) -> &'scope GcSlot<'scope, Erased> {
        debug_assert!(!self.is_leaf() && i < FAN);
        unsafe { &*self.0.as_ptr().cast::<usize>().add(1).cast::<GcSlot<'scope, Erased>>().add(i) }
    }

    fn tail_meta(self) -> &'scope IsoCell<u64> {
        debug_assert!(!self.is_leaf());
        unsafe { &*self.0.as_ptr().byte_add(Self::TAIL_META).cast::<IsoCell<u64>>() }
    }

    fn tail_slot(self) -> &'scope GcSlot<'scope, Erased> {
        debug_assert!(!self.is_leaf());
        unsafe { &*self.0.as_ptr().byte_add(Self::TAIL_SLOT).cast::<GcSlot<'scope, Erased>>() }
    }

    fn locate(self, i: usize) -> Option<(Store<'scope>, usize, usize)> {
        if self.is_leaf() {
            let cap = self.head();
            return (i < cap).then_some((self, 0, cap));
        }
        let mut s = self;
        loop {
            if s.is_leaf() {
                let cap = s.head();
                let base = i & !(FAN - 1);
                return (i - base < cap).then_some((s, base, cap));
            }
            let level = s.head() - NODE;
            let shift = FAN_BITS as usize * level;
            s = Self(s.kid((i >> shift) & (FAN - 1)).get()?);
        }
    }

    fn remember(self, mc: &Mutation<'scope>, index: usize) {
        if self.is_leaf() {
            return;
        }
        let Some((leaf, base, cap)) = self.locate(index) else {
            self.tail_slot().set(mc.mutator(), None);
            return;
        };
        let meta = u64::from(u32::try_from(base).expect("element index fits u32")) | (u64::from(u32::try_from(cap).expect("leaf capacity fits u32")) << 32);
        self.tail_slot().set(mc.mutator(), None);
        self.tail_meta().set(meta);
        self.tail_slot().set(mc.mutator(), Some(leaf.0));
    }

    pub const NODE_MARK: usize = NODE;
    pub const TAIL_META: usize = core::mem::offset_of!(Node<'static>, tail_meta);
    pub const TAIL_SLOT: usize = core::mem::offset_of!(Node<'static>, tail);

    pub fn path(self, i: usize) -> Option<StorePath> {
        let head = self.head();
        let mut kids = Vec::new();
        let mut s = self;
        let mut i_left = i;
        loop {
            if s.is_leaf() {
                let at = if kids.is_empty() { i_left } else { i_left & (FAN - 1) };
                if at >= s.head() {
                    return None;
                }
                return Some(StorePath { head, kids, cell: core::mem::size_of::<usize>() + at * core::mem::size_of::<ValueCell<'scope>>() });
            }
            let level = s.head() - NODE;
            let shift = FAN_BITS * u32::try_from(level).ok()?;
            if kids.is_empty() && (i >> (FAN_BITS * u32::try_from(level + 1).ok()?)) != 0 {
                return None;
            }
            let k = (i >> shift) & (FAN - 1);
            kids.push(core::mem::size_of::<usize>() + k * core::mem::size_of::<GcSlot<'scope, Erased>>());
            s = Self(s.kid(k).get()?);
            i_left = i;
        }
    }

    pub fn cell(self, i: usize) -> Option<&'scope ValueCell<'scope>> {
        let h = self.head();
        if h < NODE {
            return (i < h).then(|| self.leaf_cell(i));
        }
        if (i >> (FAN_BITS * u32::try_from(h - NODE + 1).ok()?)) != 0 {
            return None;
        }
        let mut s = self;
        loop {
            if s.is_leaf() {
                return Some(s.leaf_cell(i & (FAN - 1)));
            }
            let level = s.head() - NODE;
            let shift = FAN_BITS * u32::try_from(level).ok()?;
            s = Self(s.kid((i >> shift) & (FAN - 1)).get()?);
        }
    }

    fn cell_mut(self, mc: &Mutation<'scope>, i: usize) -> &'scope ValueCell<'scope> {
        if self.is_leaf() {
            return self.leaf_cell(i);
        }
        let mut s = self;
        loop {
            if s.is_leaf() {
                return s.leaf_cell(i & (FAN - 1));
            }
            let level = s.head() - NODE;
            let shift = FAN_BITS as usize * level;
            let slot = s.kid((i >> shift) & (FAN - 1));
            s = match slot.get() {
                Some(k) => Self(k),
                None => {
                    let fresh = if level == 1 { Self::leaf(mc, FAN, &[]) } else { Self::node(mc, level - 1, None) };
                    slot.set(mc.mutator(), Some(fresh.0));
                    fresh
                }
            };
        }
    }

    fn run_mut(self, mc: &Mutation<'scope>, i: usize) -> &'scope [ValueCell<'scope>] {
        let first = self.cell_mut(mc, i);
        let left = if self.is_leaf() { self.head() - i } else { FAN - (i & (FAN - 1)) };
        unsafe { core::slice::from_raw_parts(first, left) }
    }

    fn grown(self, mc: &Mutation<'scope>, need: usize, len: usize) -> Self {
        if need <= self.capacity() {
            return self;
        }
        if self.is_leaf() {
            let old: alloc::vec::Vec<Value<'scope>> = (0..len.min(self.head())).map(|i| self.leaf_cell(i).get()).collect();
            if need <= FAN {
                return Self::leaf(mc, need, &old);
            }
            let full = if self.head() == FAN { self } else { Self::leaf(mc, FAN, &old) };
            return Self::node(mc, 1, Some(full)).grown(mc, need, len);
        }
        let level = self.head() - NODE;
        Self::node(mc, level + 1, Some(self)).grown(mc, need, len)
    }

    pub fn from_values(mc: &Mutation<'scope>, values: &[Value<'scope>]) -> Option<Self> {
        if values.is_empty() {
            return None;
        }
        if values.len() <= FAN {
            return Some(Self::leaf(mc, values.len(), values));
        }
        let s = Self::leaf(mc, FAN, &values[..FAN]).grown(mc, values.len(), FAN);
        for (i, &v) in values.iter().enumerate().skip(FAN) {
            s.cell_mut(mc, i).set(mc, v);
        }
        Some(s)
    }

    pub fn reserve(mc: &Mutation<'scope>, n: usize) -> Self {
        if n <= FAN { Self::leaf(mc, n, &[]) } else { Self::leaf(mc, FAN, &[]).grown(mc, n, 0) }
    }

    pub fn put(this: Option<Self>, mc: &Mutation<'scope>, len: usize, i: usize, v: Value<'scope>) -> Self {
        if i < FAN && this.is_none_or(|s| s.is_leaf() && i >= s.head()) {
            let mut init: alloc::vec::Vec<Value<'scope>> = this.map_or_else(alloc::vec::Vec::new, |s| (0..len.min(s.head())).map(|k| s.leaf_cell(k).get()).collect());
            init.resize(i, Value::Undefined);
            init.push(v);
            return Self::leaf(mc, i + 1, &init);
        }
        let s = match this {
            Some(s) => s.grown(mc, i + 1, len),
            None => Self::leaf(mc, i + 1, &[]),
        };
        s.cell_mut(mc, i).set(mc, v);
        s
    }

    pub fn gc(self) -> Gc<'scope, Erased> {
        self.0
    }

    pub unsafe fn from_gc(g: Gc<'scope, Erased>) -> Self {
        Self(g)
    }
}

#[derive(Default)]
pub struct ValueArray<'scope> {
    len: IsoCell<u32>,
    root: GcSlot<'scope, Erased>,
}

unsafe impl crate::Trace for ValueArray<'_> {
    fn trace(&self, t: &mut crate::Tracer<'_>) {
        self.root.trace(t);
    }

    fn relocate(&mut self, t: &mut crate::Tracer<'_>) {
        self.root.relocate(t);
    }
}

#[derive(Clone, Copy)]
pub struct ElementRun<'scope> {
    root: usize,
    base: u32,
    count: u32,
    cells: *const ValueCell<'scope>,
}

impl<'scope> ElementRun<'scope> {
    #[inline]
    pub fn get(self, elements: &ValueArray<'scope>, i: usize) -> Option<Value<'scope>> {
        let at = i.checked_sub(self.base as usize)?;
        if at >= self.count as usize || i >= elements.len() {
            return None;
        }
        let root = elements.root.get()?;
        if root.as_ptr() as usize != self.root {
            return None;
        }
        Some(unsafe { (*self.cells.add(at)).get() })
    }
}

impl<'scope> ValueArray<'scope> {
    pub const LEN_OFFSET: usize = core::mem::offset_of!(ValueArray<'static>, len);
    pub const ROOT_OFFSET: usize = core::mem::offset_of!(ValueArray<'static>, root);

    #[inline]
    pub fn len(&self) -> usize {
        self.len.get() as usize
    }

    #[inline]
    fn append_cell(&self, i: usize) -> Option<&'scope ValueCell<'scope>> {
        let root = Store(self.root.get()?);
        let h = root.head();
        if h < NODE {
            return (i < h).then(|| root.leaf_cell(i));
        }
        let tail = root.tail_slot().get()?;
        let meta = root.tail_meta().get();
        let base = usize::try_from(meta & 0xFFFF_FFFF).unwrap_or(0);
        let cap = usize::try_from(meta >> 32).unwrap_or(0);
        let at = i.checked_sub(base)?;
        (at < cap).then(|| Store(tail).leaf_cell(at))
    }

    #[inline]
    pub fn run(&self, i: usize) -> Option<ElementRun<'scope>> {
        if i >= self.len() {
            return None;
        }
        let root = self.root.get()?;
        let s = Store(root);
        let base = if s.is_leaf() { 0 } else { i & !(FAN - 1) };
        let first = s.cell(base)?;
        let span = if s.is_leaf() { s.head() } else { FAN };
        let end = base.saturating_add(span).min(self.len());
        if i >= end {
            return None;
        }
        Some(ElementRun { root: root.as_ptr() as usize, base: u32::try_from(base).ok()?, count: u32::try_from(end - base).ok()?, cells: first })
    }

    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }

    fn store(&self) -> Option<Store<'scope>> {
        self.root.get().map(Store)
    }

    #[inline]
    pub fn get(&self, i: usize) -> Option<Value<'scope>> {
        if i >= self.len() {
            return None;
        }
        Some(self.store()?.cell(i).map_or(Value::Undefined, ValueCell::get))
    }

    pub fn set(&self, mc: &Mutation<'scope>, i: usize, v: Value<'scope>) {
        if i < self.len()
            && let Some(s) = self.store()
        {
            s.cell_mut(mc, i).set(mc, v);
        }
    }

    #[inline]
    fn reserve(&self, mc: &Mutation<'scope>, need: usize) -> Store<'scope> {
        let cur = self.root.get();
        if let Some(g) = cur
            && need <= Store(g).capacity()
        {
            return Store(g);
        }
        let s = match cur {
            Some(g) => Store(g),
            None => Store::leaf(mc, need, &[]),
        };
        let grown = s.grown(mc, need, self.len());
        if cur.is_none_or(|c| !Gc::ptr_eq(c, grown.0)) {
            self.root.set(mc.mutator(), Some(grown.0));
        }
        grown
    }

    #[inline]
    pub fn push(&self, mc: &Mutation<'scope>, v: Value<'scope>) {
        let i = self.len();
        let n = i + 1;
        if let Some(cell) = self.append_cell(i) {
            cell.set(mc, v);
            self.len.set(u32::try_from(n).expect("array length fits u32"));
            return;
        }
        self.reserve(mc, n).cell_mut(mc, i).set(mc, v);
        if let Some(g) = self.root.get() {
            Store(g).remember(mc, i);
        }
        self.len.set(u32::try_from(n).expect("array length fits u32"));
    }

    pub fn fill(&self, mc: &Mutation<'scope>, from: usize, to: usize, v: Value<'scope>) {
        let n = self.len();
        if from > n || from >= to {
            return;
        }
        let s = match self.root.get() {
            None => {
                let s = Store::reserve(mc, to);
                self.root.set(mc.mutator(), Some(s.0));
                s
            }
            Some(_) => self.reserve(mc, to),
        };
        let mut i = from;
        while i < to {
            let run = s.run_mut(mc, i);
            let k = run.len().min(to - i);
            for c in &run[..k] {
                c.set(mc, v);
            }
            i += k;
        }
        if to > n {
            self.len.set(u32::try_from(to).expect("array length fits u32"));
        }
    }

    pub fn pop(&self, mc: &Mutation<'scope>) -> Option<Value<'scope>> {
        let n = self.len().checked_sub(1)?;
        if let Some(cell) = self.append_cell(n) {
            let v = cell.get();
            cell.set(mc, Value::Undefined);
            self.len.set(u32::try_from(n).expect("array length fits u32"));
            return Some(v);
        }
        let v = self.get(n);
        self.truncate(mc, n);
        v
    }

    pub fn truncate(&self, mc: &Mutation<'scope>, n: usize) {
        let old = self.len();
        if n >= old {
            return;
        }
        self.len.set(u32::try_from(n).expect("array length fits u32"));
        if let Some(s) = self.store() {
            for i in n..old {
                if let Some(c) = s.cell(i) {
                    c.set(mc, Value::Undefined);
                }
            }
        }
    }

    pub fn clear(&self, mc: &Mutation<'scope>) {
        self.truncate(mc, 0);
    }

    pub fn insert(&self, mc: &Mutation<'scope>, i: usize, v: Value<'scope>) {
        let n = self.len();
        let s = self.reserve(mc, n + 1);
        for k in (i..n).rev() {
            s.cell_mut(mc, k + 1).set(mc, s.cell(k).map_or(Value::Undefined, ValueCell::get));
        }
        s.cell_mut(mc, i).set(mc, v);
        self.len.set(u32::try_from(n + 1).expect("array length fits u32"));
    }

    pub fn remove(&self, mc: &Mutation<'scope>, i: usize) -> Value<'scope> {
        let n = self.len();
        let v = self.get(i).unwrap_or_default();
        if let Some(s) = self.store() {
            for k in i + 1..n {
                s.cell_mut(mc, k - 1).set(mc, s.cell(k).map_or(Value::Undefined, ValueCell::get));
            }
        }
        self.truncate(mc, n - 1);
        v
    }

    pub fn replace(&self, mc: &Mutation<'scope>, values: &[Value<'scope>]) {
        let fresh = Store::from_values(mc, values);
        self.root.set(mc.mutator(), fresh.map(Store::gc));
        self.len.set(u32::try_from(values.len()).expect("array length fits u32"));
    }

    pub fn to_vec(&self) -> alloc::vec::Vec<Value<'scope>> {
        let n = self.len();
        let Some(s) = self.store() else { return alloc::vec::Vec::new() };
        (0..n).map(|i| s.cell(i).map_or(Value::Undefined, ValueCell::get)).collect()
    }

    pub fn iter(&self) -> impl Iterator<Item = Value<'scope>> + '_ {
        (0..self.len()).map(|i| self.get(i).unwrap_or_default())
    }
}

const _: () = assert!(core::mem::size_of::<ValueArray<'static>>() == 24);
const _: () = assert!(ValueArray::ROOT_OFFSET == 0 && ValueArray::LEN_OFFSET == 16);
const _: () = assert!(Store::TAIL_META == 8 + 128 * 16 && Store::TAIL_SLOT == Store::TAIL_META + 8);
const _: () = assert!(Store::NODE_MARK == 1 << 16);

#[cfg(test)]
mod tests {
    use core::num::NonZeroUsize;

    use crate::{Heap, Mutation, Value, ValueArray};

    crate::paragc::brand!(ArrayBrand = for<'scope> ValueArray<'scope>);

    fn number_at(a: &ValueArray<'_>, i: usize) -> f64 {
        a.get(i).and_then(Value::as_number).unwrap_or(f64::NAN)
    }

    fn gc_word(s: &crate::GcSlot<'_, crate::Erased>) -> u64 {
        unsafe { core::ptr::read(core::ptr::from_ref(s).cast::<u64>()) }
    }

    #[expect(clippy::cast_precision_loss, reason = "every index in this test is below 2^53")]
    fn expect_filled(a: &ValueArray<'_>, n: usize) {
        assert_eq!(a.len(), n, "length after growing to {n}");
        let mut seen = alloc::vec::Vec::new();
        for i in 0..n.min(256) {
            seen.push(i);
        }
        for i in [126, 127, 128, 129, 16382, 16383, 16384, 16385, 2_097_151, 2_097_152, 2_097_153] {
            if i < n {
                seen.push(i);
            }
        }
        if n > 0 {
            seen.push(n - 1);
        }
        let mut i = 256;
        while i < n {
            seen.push(i);
            i += 4096;
        }
        for i in seen {
            assert_eq!(number_at(a, i), i as f64, "element {i} of {n}");
        }
    }

    #[expect(clippy::cast_precision_loss, reason = "every index in this test is below 2^53")]
    fn push_to<'scope>(mc: &Mutation<'scope>, a: &ValueArray<'scope>, n: usize) {
        while a.len() < n {
            let i = a.len();
            a.push(mc, Value::Number(i as f64));
        }
    }

    #[expect(clippy::cast_precision_loss, reason = "every index in this test is below 2^53")]
    fn across_boundary<'scope>(mc: &Mutation<'scope>, a: &ValueArray<'scope>, low: usize, high: usize) {
        push_to(mc, a, high);
        expect_filled(a, high);
        let mut popped = alloc::vec::Vec::new();
        while a.len() > low {
            popped.push(a.pop(mc).and_then(Value::as_number).unwrap_or(f64::NAN));
        }
        let want: alloc::vec::Vec<f64> = (low..high).rev().map(|i| i as f64).collect();
        assert_eq!(popped, want, "pop walks back from {high} to {low}");
        push_to(mc, a, high + 8);
        expect_filled(a, high + 8);
    }

    #[expect(clippy::cast_precision_loss, reason = "every index in this test is below 2^53")]
    #[test]
    fn append_truncate_and_reappend_across_leaves_levels_and_collections() {
        let fresh = ValueArray::default();
        assert_eq!(gc_word(&fresh.root), 0, "an empty root slot is a zero word");
        let mut heap = Heap::new(NonZeroUsize::MIN);
        let rooted = heap.mutate(|m| {
            let mc = unsafe { Mutation::new(m, 0) };
            let g = mc.alloc(ValueArray::default());
            let a = g.get();
            push_to(&mc, a, 100);
            expect_filled(a, 100);
            let run = a.run(0).expect("a cursor over the first leaf");
            assert_eq!(run.get(a, 0).and_then(Value::as_number), Some(0.0), "the cursor sees the element it snapped");
            push_to(&mc, a, 128);
            assert_eq!(run.get(a, 100), None, "a cursor does not observe pushes past the length it snapped");
            assert_eq!(run.get(a, 0).and_then(Value::as_number), Some(0.0), "growth inside the leaf keeps the cursor");
            push_to(&mc, a, 129);
            assert_eq!(run.get(a, 0), None, "a new root invalidates the cursor");
            expect_filled(a, 129);
            across_boundary(&mc, a, 126, 129);
            a.truncate(&mc, 50);
            push_to(&mc, a, 200);
            expect_filled(a, 200);
            a.replace(&mc, &[Value::Number(1.0), Value::Number(2.0)]);
            a.push(&mc, Value::Number(3.0));
            assert_eq!(a.len(), 3, "push after replace");
            assert_eq!(number_at(a, 2), 3.0, "the element written after replace");
            a.truncate(&mc, 0);
            push_to(&mc, a, 4);
            a.insert(&mc, 1, Value::Number(9.0));
            assert_eq!(number_at(a, 1), 9.0, "insert shifts the tail");
            assert_eq!(a.remove(&mc, 1).as_number(), Some(9.0), "remove returns the element");
            expect_filled(a, 4);
            a.fill(&mc, 0, 130, Value::Number(7.0));
            assert_eq!(number_at(a, 129), 7.0, "fill grows into the next leaf");
            a.truncate(&mc, 0);
            a.replace(&mc, &[Value::Number(0.0), Value::Number(1.0), Value::Number(2.0)]);
            push_to(&mc, a, 10);
            expect_filled(a, 10);
            a.truncate(&mc, 0);
            push_to(&mc, a, 16385);
            expect_filled(a, 16385);
            m.root::<ArrayBrand>(g)
        });
        heap.collect();
        heap.mutate(|m| {
            let mc = unsafe { Mutation::new(m, 1) };
            let a = m.get(&rooted).get();
            expect_filled(a, 16385);
            a.truncate(&mc, 16380);
            push_to(&mc, a, 16390);
            expect_filled(a, 16390);
            a.truncate(&mc, 0);
            push_to(&mc, a, 2_097_153);
            expect_filled(a, 2_097_153);
        });
        heap.collect();
        heap.mutate(|m| {
            let mc = unsafe { Mutation::new(m, 2) };
            let a = m.get(&rooted).get();
            expect_filled(a, 2_097_153);
            across_boundary(&mc, a, 2_097_150, 2_097_153);
            let run = a.run(2_097_152).expect("a cursor on the newest level");
            a.push(&mc, Value::Number(a.len() as f64));
            assert_eq!(run.get(a, 2_097_152).and_then(Value::as_number), Some(2_097_152.0), "a push past the snapped run stays invisible to it");
        });
    }
}
