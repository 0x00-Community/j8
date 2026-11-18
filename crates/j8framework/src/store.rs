//! GC 堆里的值存储：属性值与数组元素放在固定容量的叶子里，大了就是 128 叉树（30 万个元素三层）。paragc 只分配定长对象，所以按容量分档。
//!
//! - 叶子 [`Leaf<N>`]：`N` 个 [`ValueCell`]（`N` = 2 / 4 / … / 128），头一个字是容量
//! - 内部节点 [`Node`]：128 个子槽（树里的叶子都是 128 格），头一个字是 [`NODE`] + 层数
//!
//! 全部 `#[repr(C)]`：不管 `N` 多大，格子都从头字之后开始，抹掉类型后也能按下标找到。读不借用、不加锁；
//! 写经 [`ValueCell::set`] 走写屏障。根换新（扩容）直接换（isolate 内单线程，见 [`IsoCell`](crate::IsoCell)）。

use crate::{Collect, Erased, Gc, GcSlot, IsoCell, Mutation, Value, ValueCell, erase};

/// 每个内部节点的子节点数 / 树里每片叶子的格数
const FAN: usize = 128;
/// 一层占的下标位数
const FAN_BITS: u32 = FAN.trailing_zeros();
/// 头字 ≥ 它的是内部节点（头字 − NODE = 层数，≥ 1）；小于它的是叶子（头字 = 容量）
const NODE: usize = 1 << 16;
/// 最小的叶子
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

/// 一棵存储（叶子或树根），抹掉了类型。
#[derive(Clone, Copy)]
pub struct Store<'gc>(Gc<'gc, Erased>);

impl<'gc> Store<'gc> {
    /// 容量为 `cap`（向上取到分档）、前几格放 `init` 的叶子
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

    /// `level` 层的空内部节点，第 0 个子节点是 `first`
    fn node(mc: &Mutation<'gc>, level: usize, first: Option<Self>) -> Self {
        let kids = core::array::from_fn(|i| GcSlot::from(if i == 0 { first.map(|s| s.0) } else { None }));
        Self(erase(mc.alloc(Node { head: NODE + level, kids })))
    }

    fn head(self) -> usize {
        // SAFETY: 叶子与节点都是 repr(C)、头一个字是 usize
        unsafe { *self.0.as_ptr().cast::<usize>() }
    }

    fn is_leaf(self) -> bool {
        self.head() < NODE
    }

    pub fn capacity(self) -> usize {
        let h = self.head();
        if h < NODE { h } else { FAN.pow(u32::try_from(h - NODE + 1).unwrap_or(u32::MAX)) }
    }

    /// 叶子的第 `i` 格（`i` < 容量）
    fn leaf_cell(self, i: usize) -> &'gc ValueCell<'gc> {
        debug_assert!(self.is_leaf() && i < self.head());
        // SAFETY: repr(C) 的叶子里格子紧跟头字；i 在容量之内；对象在本次 mutate 里活着
        unsafe { &*self.0.as_ptr().cast::<usize>().add(1).cast::<ValueCell<'gc>>().add(i) }
    }

    /// 节点的第 `i` 个子槽
    fn kid(self, i: usize) -> &'gc GcSlot<'gc, Erased> {
        debug_assert!(!self.is_leaf() && i < FAN);
        // SAFETY: repr(C) 的节点里子槽紧跟头字
        unsafe { &*self.0.as_ptr().cast::<usize>().add(1).cast::<GcSlot<'gc, Erased>>().add(i) }
    }

    /// 第 `i` 格（路径上缺节点就是 `None`）
    pub fn cell(self, i: usize) -> Option<&'gc ValueCell<'gc>> {
        let h = self.head();
        if h < NODE {
            return (i < h).then(|| self.leaf_cell(i));
        }
        // Past this tree's capacity: shifting out every level leaves something
        if (i >> (FAN_BITS * u32::try_from(h - NODE + 1).ok()?)) != 0 {
            return None;
        }
        let mut s = self;
        loop {
            if s.is_leaf() {
                // Leaves in a tree are full ones; the low bits pick the cell
                return Some(s.leaf_cell(i & (FAN - 1)));
            }
            let level = s.head() - NODE;
            let shift = FAN_BITS * u32::try_from(level).ok()?;
            s = Self(s.kid((i >> shift) & (FAN - 1)).get()?);
        }
    }

    /// 第 `i` 格，路径上缺的节点就地补上（`i` < 容量）
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
            let shift = FAN_BITS * u32::try_from(level).expect("tree depth");
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

    /// 第 `i` 格起到它所在叶子末尾的那几格（路径上缺的节点同 [`Self::cell_mut`] 补上；`i` < 容量）
    fn run_mut(self, mc: &Mutation<'gc>, i: usize) -> &'gc [ValueCell<'gc>] {
        let first = self.cell_mut(mc, i);
        let left = if self.is_leaf() { self.head() - i } else { FAN - (i & (FAN - 1)) };
        // SAFETY: 叶子的格子在 repr(C) 里连续排着，first 是其中第 i（树里是 i mod FAN）格，到叶子末尾还有 left 格
        unsafe { core::slice::from_raw_parts(first, left) }
    }

    /// 能放下 `need` 格的存储：够了就是自己，否则换一棵更大的（前 `len` 格搬过去 / 挂到新根底下）
    fn grown(self, mc: &Mutation<'gc>, need: usize, len: usize) -> Self {
        if need <= self.capacity() {
            return self;
        }
        if self.is_leaf() {
            let old: alloc::vec::Vec<Value<'gc>> = (0..len.min(self.head())).map(|i| self.leaf_cell(i).get()).collect();
            if need <= FAN {
                return Self::leaf(mc, need, &old);
            }
            // 叶子满 32 格才能挂进树
            let full = if self.head() == FAN { self } else { Self::leaf(mc, FAN, &old) };
            return Self::node(mc, 1, Some(full)).grown(mc, need, len);
        }
        let level = self.head() - NODE;
        Self::node(mc, level + 1, Some(self)).grown(mc, need, len)
    }

    /// 由值建一棵新存储
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

    /// 能放 `n` 格的空存储（对象字面量预留）
    pub fn reserve(mc: &Mutation<'gc>, n: usize) -> Self {
        if n <= FAN { Self::leaf(mc, n, &[]) } else { Self::leaf(mc, FAN, &[]).grown(mc, n, 0) }
    }

    /// 把第 `i` 格设成 `v`：放不下就换一棵更大的（前 `len` 格搬过去）；返回现在的存储
    pub fn put(this: Option<Self>, mc: &Mutation<'gc>, len: usize, i: usize, v: Value<'gc>) -> Self {
        // A new leaf is built with its values (no write barrier, nothing for the remembered set)
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

    /// # Safety
    /// `g` 是 [`Store::gc`] 给出的（叶子或节点）。
    pub unsafe fn from_gc(g: Gc<'gc, Erased>) -> Self {
        Self(g)
    }
}

/// 可增长的值数组（数组的稠密部分）：长度 + 存储根。
#[derive(Default)]
pub struct ValueArray<'gc> {
    len: IsoCell<u32>,
    root: GcSlot<'gc, Erased>,
}

// SAFETY: 根槽是唯一的边（长度不是边）
unsafe impl crate::Trace for ValueArray<'_> {
    fn trace(&self, t: &mut crate::Tracer<'_>) {
        self.root.trace(t);
    }

    fn relocate(&mut self, t: &mut crate::Tracer<'_>) {
        self.root.relocate(t);
    }
}

impl<'gc> ValueArray<'gc> {
    pub fn len(&self) -> usize {
        self.len.get() as usize
    }

    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }

    fn store(&self) -> Option<Store<'gc>> {
        self.root.get().map(Store)
    }

    pub fn get(&self, i: usize) -> Option<Value<'gc>> {
        if i >= self.len() {
            return None;
        }
        Some(self.store()?.cell(i).map_or(Value::Undefined, ValueCell::get))
    }

    /// 改第 `i` 个（`i` < 长度；越界什么都不做）
    pub fn set(&self, mc: &Mutation<'gc>, i: usize, v: Value<'gc>) {
        if i < self.len()
            && let Some(s) = self.store()
        {
            s.cell_mut(mc, i).set(mc, v);
        }
    }

    /// 容量至少 `need` 的存储（扩容就换根）
    fn reserve(&self, mc: &Mutation<'gc>, need: usize) -> Store<'gc> {
        let cur = self.root.get();
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

    pub fn push(&self, mc: &Mutation<'gc>, v: Value<'gc>) {
        let i = self.len();
        self.reserve(mc, i + 1).cell_mut(mc, i).set(mc, v);
        self.len.set(u32::try_from(i + 1).expect("array length fits u32"));
    }

    /// 第 `from` 到 `to` 个（不含）都设成 `v`，长度不够就接长到 `to`（`from` ≤ 长度）：一片叶子走一次树
    pub fn fill(&self, mc: &Mutation<'gc>, from: usize, to: usize, v: Value<'gc>) {
        let n = self.len();
        if from > n || from >= to {
            return;
        }
        // 还没有存储时一次建好能放 `to` 格的（reserve 只建一片叶子）
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

    /// 截到 `n` 个；空出来的格子清成 `undefined`（别让回收器还看见旧值）
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

    /// 在 `i` 处插入（后面的往后挪）
    pub fn insert(&self, mc: &Mutation<'gc>, i: usize, v: Value<'gc>) {
        let n = self.len();
        let s = self.reserve(mc, n + 1);
        for k in (i..n).rev() {
            s.cell_mut(mc, k + 1).set(mc, s.cell(k).map_or(Value::Undefined, ValueCell::get));
        }
        s.cell_mut(mc, i).set(mc, v);
        self.len.set(u32::try_from(n + 1).expect("array length fits u32"));
    }

    /// 删掉第 `i` 个（后面的往前挪）
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

    /// 全部换成 `values`
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
