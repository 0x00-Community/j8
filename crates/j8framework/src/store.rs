use alloc::vec::Vec;

use crate::{Collect, Erased, Gc, GcSlot, IsoCell, Mutation, Value, ValueCell, erase};

const FAN: usize = 128;

const FAN_BITS: u32 = FAN.trailing_zeros();

const NODE: usize = 1 << 16;

const MIN_LEAF: usize = 2;

#[repr(C)]
#[derive(Collect)]
struct Leaf<'gc, const N: usize> {
    head: usize,
    cells: [ValueCell<'gc>; N],
}

#[repr(C)]
#[derive(Collect)]
struct Node<'gc> {
    head: usize,
    kids: [GcSlot<'gc, Erased>; FAN],
}

#[derive(Clone, Copy)]
pub struct Store<'gc>(Gc<'gc, Erased>);

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct StorePath {
    pub head: usize,
    pub kids: Vec<usize>,
    pub cell: usize,
}

impl<'gc> Store<'gc> {
    fn leaf(mc: &Mutation<'gc>, cap: usize, init: &[Value<'gc>]) -> Self {
        fn make<'gc, const N: usize>(mc: &Mutation<'gc>, init: &[Value<'gc>]) -> Gc<'gc, Erased> {
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

    fn node(mc: &Mutation<'gc>, level: usize, first: Option<Self>) -> Self {
        let kids = core::array::from_fn(|i| GcSlot::from(if i == 0 { first.map(|s| s.0) } else { None }));
        Self(erase(mc.alloc(Node { head: NODE + level, kids })))
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

    fn leaf_cell(self, i: usize) -> &'gc ValueCell<'gc> {
        debug_assert!(self.is_leaf() && i < self.head());
        unsafe { &*self.0.as_ptr().cast::<usize>().add(1).cast::<ValueCell<'gc>>().add(i) }
    }

    fn kid(self, i: usize) -> &'gc GcSlot<'gc, Erased> {
        debug_assert!(!self.is_leaf() && i < FAN);
        unsafe { &*self.0.as_ptr().cast::<usize>().add(1).cast::<GcSlot<'gc, Erased>>().add(i) }
    }

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
                return Some(StorePath { head, kids, cell: core::mem::size_of::<usize>() + at * core::mem::size_of::<ValueCell<'gc>>() });
            }
            let level = s.head() - NODE;
            let shift = FAN_BITS * u32::try_from(level).ok()?;
            if kids.is_empty() && (i >> (FAN_BITS * u32::try_from(level + 1).ok()?)) != 0 {
                return None;
            }
            let k = (i >> shift) & (FAN - 1);
            kids.push(core::mem::size_of::<usize>() + k * core::mem::size_of::<GcSlot<'gc, Erased>>());
            s = Self(s.kid(k).get()?);
            i_left = i;
        }
    }

    pub fn cell(self, i: usize) -> Option<&'gc ValueCell<'gc>> {
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

    fn cell_mut(self, mc: &Mutation<'gc>, i: usize) -> &'gc ValueCell<'gc> {
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

    fn run_mut(self, mc: &Mutation<'gc>, i: usize) -> &'gc [ValueCell<'gc>] {
        let first = self.cell_mut(mc, i);
        let left = if self.is_leaf() { self.head() - i } else { FAN - (i & (FAN - 1)) };
        unsafe { core::slice::from_raw_parts(first, left) }
    }

    fn grown(self, mc: &Mutation<'gc>, need: usize, len: usize) -> Self {
        if need <= self.capacity() {
            return self;
        }
        if self.is_leaf() {
            let old: alloc::vec::Vec<Value<'gc>> = (0..len.min(self.head())).map(|i| self.leaf_cell(i).get()).collect();
            if need <= FAN {
                return Self::leaf(mc, need, &old);
            }
            let full = if self.head() == FAN { self } else { Self::leaf(mc, FAN, &old) };
            return Self::node(mc, 1, Some(full)).grown(mc, need, len);
        }
        let level = self.head() - NODE;
        Self::node(mc, level + 1, Some(self)).grown(mc, need, len)
    }

    pub fn from_values(mc: &Mutation<'gc>, values: &[Value<'gc>]) -> Option<Self> {
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

    pub fn reserve(mc: &Mutation<'gc>, n: usize) -> Self {
        if n <= FAN { Self::leaf(mc, n, &[]) } else { Self::leaf(mc, FAN, &[]).grown(mc, n, 0) }
    }

    pub fn put(this: Option<Self>, mc: &Mutation<'gc>, len: usize, i: usize, v: Value<'gc>) -> Self {
        if i < FAN && this.is_none_or(|s| s.is_leaf() && i >= s.head()) {
            let mut init: alloc::vec::Vec<Value<'gc>> = this.map_or_else(alloc::vec::Vec::new, |s| (0..len.min(s.head())).map(|k| s.leaf_cell(k).get()).collect());
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

    pub fn gc(self) -> Gc<'gc, Erased> {
        self.0
    }

    pub unsafe fn from_gc(g: Gc<'gc, Erased>) -> Self {
        Self(g)
    }
}

#[derive(Default)]
pub struct ValueArray<'gc> {
    len: IsoCell<u32>,
    root: GcSlot<'gc, Erased>,
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
pub struct ElementRun<'gc> {
    root: usize,
    base: u32,
    count: u32,
    cells: *const ValueCell<'gc>,
}

impl<'gc> ElementRun<'gc> {
    #[inline]
    pub fn get(self, elements: &ValueArray<'gc>, i: usize) -> Option<Value<'gc>> {
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

impl<'gc> ValueArray<'gc> {
    #[inline]
    pub fn len(&self) -> usize {
        self.len.get() as usize
    }

    #[inline]
    pub fn run(&self, i: usize) -> Option<ElementRun<'gc>> {
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

    fn store(&self) -> Option<Store<'gc>> {
        self.root.get().map(Store)
    }

    #[inline]
    pub fn get(&self, i: usize) -> Option<Value<'gc>> {
        if i >= self.len() {
            return None;
        }
        Some(self.store()?.cell(i).map_or(Value::Undefined, ValueCell::get))
    }

    pub fn set(&self, mc: &Mutation<'gc>, i: usize, v: Value<'gc>) {
        if i < self.len()
            && let Some(s) = self.store()
        {
            s.cell_mut(mc, i).set(mc, v);
        }
    }

    #[inline]
    fn reserve(&self, mc: &Mutation<'gc>, need: usize) -> Store<'gc> {
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
    pub fn push(&self, mc: &Mutation<'gc>, v: Value<'gc>) {
        let i = self.len();
        self.reserve(mc, i + 1).cell_mut(mc, i).set(mc, v);
        self.len.set(u32::try_from(i + 1).expect("array length fits u32"));
    }

    pub fn fill(&self, mc: &Mutation<'gc>, from: usize, to: usize, v: Value<'gc>) {
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

    pub fn pop(&self, mc: &Mutation<'gc>) -> Option<Value<'gc>> {
        let n = self.len().checked_sub(1)?;
        let v = self.get(n);
        self.truncate(mc, n);
        v
    }

    pub fn truncate(&self, mc: &Mutation<'gc>, n: usize) {
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

    pub fn clear(&self, mc: &Mutation<'gc>) {
        self.truncate(mc, 0);
    }

    pub fn insert(&self, mc: &Mutation<'gc>, i: usize, v: Value<'gc>) {
        let n = self.len();
        let s = self.reserve(mc, n + 1);
        for k in (i..n).rev() {
            s.cell_mut(mc, k + 1).set(mc, s.cell(k).map_or(Value::Undefined, ValueCell::get));
        }
        s.cell_mut(mc, i).set(mc, v);
        self.len.set(u32::try_from(n + 1).expect("array length fits u32"));
    }

    pub fn remove(&self, mc: &Mutation<'gc>, i: usize) -> Value<'gc> {
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

    pub fn replace(&self, mc: &Mutation<'gc>, values: &[Value<'gc>]) {
        let fresh = Store::from_values(mc, values);
        self.root.set(mc.mutator(), fresh.map(Store::gc));
        self.len.set(u32::try_from(values.len()).expect("array length fits u32"));
    }

    pub fn to_vec(&self) -> alloc::vec::Vec<Value<'gc>> {
        let n = self.len();
        let Some(s) = self.store() else { return alloc::vec::Vec::new() };
        (0..n).map(|i| s.cell(i).map_or(Value::Undefined, ValueCell::get)).collect()
    }

    pub fn iter(&self) -> impl Iterator<Item = Value<'gc>> + '_ {
        (0..self.len()).map(|i| self.get(i).unwrap_or_default())
    }
}
