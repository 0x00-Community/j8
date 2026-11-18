//! j8 的 GC 层：全部建在 paragc 的公开接口上（`Gc` / `Slot` / `Mutator` / `Root`），不碰它的内部。
//!
//! paragc 里对象里能改的边只有 [`Slot`]（带写屏障），所以 j8 的可变堆数据分两种放：
//! - [`GcCell`]：一整块可变数据（`ObjectData` 等）。当前版本放在槽指着的快照里；快照是本轮（上次回收之后）新分配的就
//!   原地改，否则先挪进一份新快照再改，槽换过去时写屏障就记下了它。每块数据每个回收周期最多挪一次
//! - [`ValueCell`]：一个 `Value`（环境槽等），字 + 槽
//!
//! 一个 isolate 同一时刻只在一个线程上跑 JS（V8 的模型），它堆里的对象只由这个线程读写，所以借用标志、标量字段都是
//! 普通读写（[`IsoCell`]），⛔ 原子读改写。只有 paragc 的 [`Slot`] 自带原子（写屏障要）。
//!
//! 原地改年轻快照不经过写屏障，所以 j8 的堆只用停顿式回收（`collect_if_needed` / `collect`），⛔ `mark_concurrently`。
//!
//! 写屏障记下的是槽的地址：⛔ 给还没放进堆的值写槽（先分配进堆再填）。
//!
//! 年轻回收会搬存活的年轻对象（paragc 缺省的 `YoungEvacuation::SkipRemembered`：老对象直接指着的不动，被搬的对象的
//! 字段都走 `relocate`），`ValueCell` 字里的地址随之改写。⛔ `YoungEvacuation::Full`：它会改老对象槽里的地址，
//! 但不知道 `ValueCell` 字里另存的那一份。
use core::cell::UnsafeCell;
use core::fmt;
use core::mem::ManuallyDrop;
use core::ops::{Deref, DerefMut};
use core::ptr::NonNull;

/// 堆类型要实现的 trait 是 paragc 的 `Trace`，用 `#[derive(Collect)]` 生成
pub use j8macros::Collect;
pub use paragc::{Brand, Gc, Heap, Mutator, Root, Slot, Tlab, Trace, Tracer};

use crate::Value;

/// 地址 ⇒ `Gc`。
///
/// # Safety
/// `ptr` 是本堆里、当前 mutate 品牌下一个活着的 `T` 的数据指针（某个 `Gc::as_ptr` 的结果）。
#[inline]
pub unsafe fn gc_from_raw<'gc, T>(ptr: *const T) -> Gc<'gc, T> {
    // SAFETY: 见上
    unsafe { Gc::from_raw(ptr) }
}

/// 一次 mutate 的令牌：paragc 的 [`Mutator`] 加上堆的回收代号（[`GcCell`] 据此判断快照是不是本轮新分配的）。
pub struct Mutation<'gc> {
    m: NonNull<Mutator<'gc>>,
    generation: u64,
}

impl<'gc> Mutation<'gc> {
    /// # Safety
    /// 只能在 `Heap::mutate` 的闭包里、用这次的 `m` 构造（`'gc` 品牌保证它逃不出闭包，`m` 一直有效）；
    /// `generation` 是这个堆完成过的回收次数。
    pub unsafe fn new(m: &Mutator<'gc>, generation: u64) -> Self {
        Self { m: NonNull::from(m), generation }
    }

    #[inline]
    pub fn mutator(&self) -> &Mutator<'gc> {
        // SAFETY: 见 new：Mutation 活着时 m 一直有效
        unsafe { self.m.as_ref() }
    }

    #[inline]
    pub fn alloc<T: Trace + 'gc>(&self, v: T) -> Gc<'gc, T> {
        self.mutator().alloc(v)
    }

    pub fn generation(&self) -> u64 {
        self.generation
    }
}

/// 抹掉类型的堆对象：只用来把不同类型的 `Gc` 放进同一种槽（回收时按对象头里的真实类型追踪）。
pub struct Erased(());

// SAFETY: 从不被直接追踪：槽里的对象由回收器按对象头访问
unsafe impl Trace for Erased {
    fn trace(&self, _: &mut Tracer<'_>) {}

    fn relocate(&mut self, _: &mut Tracer<'_>) {}
}

/// 任意 `Gc` ⇒ 抹掉类型
#[inline]
pub fn erase<T>(g: Gc<'_, T>) -> Gc<'_, Erased> {
    // SAFETY: 同一个活对象，只换了静态类型
    unsafe { gc_from_raw(g.as_ptr().cast()) }
}

// ---- IsoCell ----

/// isolate 内单线程的单元：普通读写，没有原子操作。
///
/// paragc 要求堆类型 `Send + Sync`，这里按契约声明：一个 isolate 同一时刻只在一个线程上跑（整体挪到别的线程可以，
/// 两个线程同时进不行），它堆里的 `IsoCell` 只由当前线程读写；回收线程只在停顿时读（这时没有 mutator）。
#[derive(Default)]
#[repr(transparent)]
pub struct IsoCell<T>(core::cell::Cell<T>);

// SAFETY: 见类型说明：同一时刻只有 isolate 的当前线程写，回收停顿时多个回收线程只读
unsafe impl<T: Copy> Sync for IsoCell<T> {}
// SAFETY: 随 isolate 整体挪到别的线程
unsafe impl<T: Copy> Send for IsoCell<T> {}

impl<T: Copy> IsoCell<T> {
    pub const fn new(v: T) -> Self {
        Self(core::cell::Cell::new(v))
    }

    #[inline]
    pub fn get(&self) -> T {
        self.0.get()
    }

    #[inline]
    pub fn set(&self, v: T) {
        self.0.set(v);
    }

    #[inline]
    pub fn get_mut(&mut self) -> &mut T {
        self.0.get_mut()
    }
}

// SAFETY: 只放标量 / 地址字（`Copy`），没有 Gc 边
unsafe impl<T: Copy + Send + 'static> Trace for IsoCell<T> {
    const NO_EDGES: bool = true;
    fn trace(&self, _: &mut Tracer<'_>) {}
}

impl<T: Copy + fmt::Debug> fmt::Debug for IsoCell<T> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        fmt::Debug::fmt(&self.get(), f)
    }
}

// ---- GcCell ----

/// [`GcCell`] 的一份快照。被挪走后（`moved`）只剩空壳：不追踪、不析构。
struct Snap<T> {
    generation: u64,
    moved: IsoCell<bool>,
    data: UnsafeCell<ManuallyDrop<T>>,
}

// SAFETY: data 只在 GcCell 的借用标志保护下、由 isolate 的当前线程访问（见 IsoCell）
unsafe impl<T: Send + Sync> Sync for Snap<T> {}

impl<T> Drop for Snap<T> {
    fn drop(&mut self) {
        if !*self.moved.get_mut() {
            // SAFETY: 没被挪走，归自己
            unsafe { ManuallyDrop::drop(self.data.get_mut()) };
        }
    }
}

// SAFETY: 没挪走时逐个追踪数据；回收时没有 mutator，读数据不冲突。Drop 只析构自己的数据、不碰别的对象
unsafe impl<T: Trace> Trace for Snap<T> {
    const NO_EDGES: bool = T::NO_EDGES;

    fn trace(&self, t: &mut Tracer<'_>) {
        if !self.moved.get() {
            // SAFETY: 回收或释放死对象时没有人持有可变借用
            unsafe { &*self.data.get() }.trace(t);
        }
    }

    fn relocate(&mut self, t: &mut Tracer<'_>) {
        if !*self.moved.get_mut() {
            self.data.get_mut().relocate(t);
        }
    }
}

/// 借用状态：最高位 = 有人在写，其余位 = 读者数
const WRITING: usize = 1 << (usize::BITS - 1);

/// 堆里的 `RefCell`（isolate 内单线程，见 [`IsoCell`]；冲突时报错），可变借用经 [`Mutation`] 保住写屏障。
/// 放在 `Gc<GcCell<T>>` 里用。
pub struct GcCell<'gc, T> {
    state: IsoCell<usize>,
    snap: Slot<'gc, Snap<T>>,
}

impl<'gc, T: Trace + 'gc> GcCell<'gc, T> {
    pub fn new(mc: &Mutation<'gc>, t: T) -> Self {
        let snap = mc.alloc(Snap { generation: mc.generation, moved: IsoCell::new(false), data: UnsafeCell::new(ManuallyDrop::new(t)) });
        Self { state: IsoCell::new(0), snap: Slot::from(snap) }
    }

    /// 还没有值（[`GcCell::empty`]）：读者自己造缺省值，第一次可变借用时才分配快照
    pub fn empty() -> Self {
        Self { state: IsoCell::new(0), snap: Slot::default() }
    }

    /// 有没有值（空的单元由持有者解释成某个缺省值）
    #[inline]
    pub fn has_value(&self) -> bool {
        self.snap.get().is_some()
    }

    /// 当前快照
    fn current(&self) -> Gc<'gc, Snap<T>> {
        self.snap.get().expect("borrowed an empty GcCell (check has_value first)")
    }

    pub fn try_borrow(&self) -> Option<CellRef<'_, T>> {
        let s = self.state.get();
        if s & WRITING != 0 {
            return None;
        }
        self.state.set(s + 1);
        // SAFETY: 读者计数挂着，没有写者
        Some(CellRef { state: &self.state, data: unsafe { &*self.current().get().data.get() } })
    }

    pub fn borrow(&self) -> CellRef<'_, T> {
        self.try_borrow().expect("GcCell 已被可变借用")
    }

    /// 可变借用：快照是本轮新分配的就原地改，否则挪进新快照（槽换过去，写屏障记下）
    pub fn try_borrow_mut<'c>(&'c self, mc: &Mutation<'gc>) -> Option<CellMut<'c, T>> {
        self.try_borrow_mut_or(mc, || unreachable!("borrowed an empty GcCell mutably without an initial value"))
    }

    /// 同 [`GcCell::try_borrow_mut`]；空的单元先放进 `init()`
    pub fn try_borrow_mut_or<'c>(&'c self, mc: &Mutation<'gc>, init: impl FnOnce() -> T) -> Option<CellMut<'c, T>> {
        if self.state.get() != 0 {
            return None;
        }
        self.state.set(WRITING);
        let Some(mut snap) = self.snap.get() else {
            let snap = mc.alloc(Snap { generation: mc.generation, moved: IsoCell::new(false), data: UnsafeCell::new(ManuallyDrop::new(init())) });
            self.snap.set(mc.mutator(), Some(snap));
            // SAFETY: 独占（WRITING）
            return Some(CellMut { state: &self.state, data: unsafe { &mut *snap.get().data.get() } });
        };
        if snap.generation != mc.generation {
            // SAFETY: 独占（WRITING）；旧快照标成已挪走，之后不再析构 / 追踪它的数据
            let data = unsafe { ManuallyDrop::take(&mut *snap.data.get()) };
            snap.moved.set(true);
            snap = mc.alloc(Snap { generation: mc.generation, moved: IsoCell::new(false), data: UnsafeCell::new(ManuallyDrop::new(data)) });
            self.snap.set(mc.mutator(), Some(snap));
        }
        // SAFETY: 独占（WRITING）
        Some(CellMut { state: &self.state, data: unsafe { &mut *snap.get().data.get() } })
    }

    pub fn borrow_mut<'c>(&'c self, mc: &Mutation<'gc>) -> CellMut<'c, T> {
        self.try_borrow_mut(mc).expect("GcCell 已被借用")
    }

    /// 独占时直接改（还没放进堆，或回收时）
    pub fn get_mut(&mut self) -> &mut T {
        let snap = self.current();
        // SAFETY: &mut self ⇒ 没有借用
        unsafe { &mut *snap.get().data.get() }
    }
}

// SAFETY: 槽就是唯一的边
unsafe impl<'gc, T: Trace + 'gc> Trace for GcCell<'gc, T> {
    fn trace(&self, t: &mut Tracer<'_>) {
        self.snap.trace(t);
    }

    fn relocate(&mut self, t: &mut Tracer<'_>) {
        self.snap.relocate(t);
    }
}

impl<'gc, T: Trace + fmt::Debug + 'gc> fmt::Debug for GcCell<'gc, T> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        if !self.has_value() {
            return f.write_str("GcCell(<empty>)");
        }
        match self.try_borrow() {
            Some(r) => fmt::Debug::fmt(&*r, f),
            None => f.write_str("GcCell(<borrowed>)"),
        }
    }
}

/// [`GcCell`] 的共享借用
pub struct CellRef<'c, T> {
    state: &'c IsoCell<usize>,
    data: &'c T,
}

impl<T> Deref for CellRef<'_, T> {
    type Target = T;

    fn deref(&self) -> &T {
        self.data
    }
}

impl<T> Drop for CellRef<'_, T> {
    fn drop(&mut self) {
        self.state.set(self.state.get() - 1);
    }
}

/// [`GcCell`] 的可变借用
pub struct CellMut<'c, T> {
    state: &'c IsoCell<usize>,
    data: &'c mut T,
}

impl<T> Deref for CellMut<'_, T> {
    type Target = T;

    fn deref(&self) -> &T {
        self.data
    }
}

impl<T> DerefMut for CellMut<'_, T> {
    fn deref_mut(&mut self) -> &mut T {
        self.data
    }
}

impl<T> Drop for CellMut<'_, T> {
    fn drop(&mut self) {
        self.state.set(0);
    }
}

// ---- ValueCell ----

/// 一个可变的 `Value`：读只看字；指针另外放进槽里让回收器看见（并经它走写屏障）。
///
/// 写：先写字，槽里不是同一个指针时再改槽（isolate 内单线程，见 [`IsoCell`]），所以回收时槽一定盖住了字里的指针。
pub struct ValueCell<'gc> {
    bits: IsoCell<u64>,
    ptr: Slot<'gc, Erased>,
}

impl<'gc> ValueCell<'gc> {
    pub fn new(v: Value<'gc>) -> Self {
        Self { bits: IsoCell::new(v.bits()), ptr: Slot::from(v.erased()) }
    }

    #[inline]
    pub fn get(&self) -> Value<'gc> {
        // SAFETY: 字里的指针总被槽盖住（见类型说明），对象活着
        unsafe { Value::from_bits(self.bits.get()) }
    }

    #[inline]
    pub fn set(&self, mc: &Mutation<'gc>, v: Value<'gc>) {
        self.bits.set(v.bits());
        let same = match (v.erased(), self.ptr.get()) {
            (None, None) => true,
            (Some(a), Some(b)) => Gc::ptr_eq(a, b),
            _ => false,
        };
        if !same {
            self.ptr.set(mc.mutator(), v.erased());
        }
    }

    /// 独占时写（还没放进堆）
    pub fn set_mut(&mut self, v: Value<'gc>) {
        *self = Self::new(v);
    }
}

// SAFETY: 槽是唯一的边；重定位时把搬走后的地址同步回字里
unsafe impl Trace for ValueCell<'_> {
    fn trace(&self, t: &mut Tracer<'_>) {
        self.ptr.trace(t);
    }

    fn relocate(&mut self, t: &mut Tracer<'_>) {
        self.ptr.relocate(t);
        // SAFETY: 回收时没有 mutator；字里是指针时槽里就是它（槽里多出来的指针只是多保活）
        let v = unsafe { Value::from_bits(*self.bits.get_mut()) };
        if v.erased().is_some()
            && let Some(p) = self.ptr.get()
        {
            *self.bits.get_mut() = v.with_address(p.as_ptr().cast()).bits();
        }
    }
}

impl fmt::Debug for ValueCell<'_> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        fmt::Debug::fmt(&self.get(), f)
    }
}

// ---- Cow ----

/// 写时复制的单元（少写多读的数据）：读是一次原子读拿到不可变的当前版本；写是复制、改、比较交换（别人先换了就重来）。
pub struct Cow<'gc, T>(Slot<'gc, T>);

impl<'gc, T: Trace + 'gc> Cow<'gc, T> {
    pub fn new(mc: &Mutation<'gc>, t: T) -> Self {
        Self(Slot::from(mc.alloc(t)))
    }

    /// 当前版本（整个 mutate 里都有效）
    pub fn get(&self) -> &'gc T {
        self.0.get().expect("Cow 总有值").get()
    }

    /// 换成 `f(当前)`，同时返回 `f` 的第二个结果。`f` 可能跑不止一次。
    pub fn update_with<R>(&self, mc: &Mutation<'gc>, mut f: impl FnMut(&T) -> (T, R)) -> R {
        let mut cur = self.0.get();
        loop {
            let (next, r) = f(cur.expect("Cow 总有值").get());
            match self.0.compare_exchange(mc.mutator(), cur, Some(mc.alloc(next))) {
                Ok(_) => return r,
                Err(now) => cur = now,
            }
        }
    }

    pub fn update(&self, mc: &Mutation<'gc>, mut f: impl FnMut(&T) -> T) {
        self.update_with(mc, |t| (f(t), ()));
    }

    pub fn set(&self, mc: &Mutation<'gc>, t: T) {
        self.0.set(mc.mutator(), Some(mc.alloc(t)));
    }
}

// SAFETY: 槽是唯一的边
unsafe impl<'gc, T: Trace + 'gc> Trace for Cow<'gc, T> {
    fn trace(&self, t: &mut Tracer<'_>) {
        self.0.trace(t);
    }

    fn relocate(&mut self, t: &mut Tracer<'_>) {
        self.0.relocate(t);
    }
}

// ---- GcQueue ----

/// [`GcQueue`] 的节点：第一个是不带值的哑节点
struct QNode<'gc, T> {
    value: Option<T>,
    next: Slot<'gc, QNode<'gc, T>>,
}

// SAFETY: 逐个字段
unsafe impl<'gc, T: Trace + 'gc> Trace for QNode<'gc, T> {
    fn trace(&self, t: &mut Tracer<'_>) {
        self.value.trace(t);
        self.next.trace(t);
    }

    fn relocate(&mut self, t: &mut Tracer<'_>) {
        self.value.relocate(t);
        self.next.relocate(t);
    }
}

/// 堆里的 Michael–Scott 无锁 FIFO：任何线程都能同时进出；出队的节点交给回收器（没有 ABA）。
pub struct GcQueue<'gc, T> {
    head: Slot<'gc, QNode<'gc, T>>,
    tail: Slot<'gc, QNode<'gc, T>>,
}

impl<'gc, T: Trace + Clone + 'gc> GcQueue<'gc, T> {
    pub fn new(mc: &Mutation<'gc>) -> Self {
        let dummy = mc.alloc(QNode { value: None, next: Slot::default() });
        Self { head: Slot::from(dummy), tail: Slot::from(dummy) }
    }

    fn node(s: &Slot<'gc, QNode<'gc, T>>) -> Gc<'gc, QNode<'gc, T>> {
        s.get().expect("队列总有哑节点")
    }

    pub fn push(&self, mc: &Mutation<'gc>, value: T) {
        let m = mc.mutator();
        let node = mc.alloc(QNode { value: Some(value), next: Slot::default() });
        loop {
            let last = Self::node(&self.tail);
            match last.get().next.get() {
                None => {
                    if last.get().next.compare_exchange(m, None, Some(node)).is_ok() {
                        let _ = self.tail.compare_exchange(m, Some(last), Some(node));
                        return;
                    }
                }
                // 尾巴落后了：帮它往前挪
                Some(n) => {
                    let _ = self.tail.compare_exchange(m, Some(last), Some(n));
                }
            }
        }
    }

    pub fn pop(&self, mc: &Mutation<'gc>) -> Option<T> {
        let m = mc.mutator();
        loop {
            let first = Self::node(&self.head);
            let last = Self::node(&self.tail);
            let next = first.get().next.get()?;
            if Gc::ptr_eq(first, last) {
                let _ = self.tail.compare_exchange(m, Some(last), Some(next));
                continue;
            }
            let value = next.get().value.clone();
            if self.head.compare_exchange(m, Some(first), Some(next)).is_ok() {
                return value;
            }
        }
    }

    pub fn is_empty(&self) -> bool {
        Self::node(&self.head).get().next.get().is_none()
    }
}

// SAFETY: 两个槽
unsafe impl<'gc, T: Trace + 'gc> Trace for GcQueue<'gc, T> {
    fn trace(&self, t: &mut Tracer<'_>) {
        self.head.trace(t);
        self.tail.trace(t);
    }

    fn relocate(&mut self, t: &mut Tracer<'_>) {
        self.head.relocate(t);
        self.tail.relocate(t);
    }
}

// ---- Static ----

/// 原样放进堆里的 `'static` 数据（不含 `Gc`，不追踪）。
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, PartialOrd, Ord, Hash)]
#[repr(transparent)]
pub struct Static<T: ?Sized>(pub T);

// SAFETY: 'static 数据里没有 Gc
unsafe impl<T: Send + Sync + 'static> Trace for Static<T> {
    const NO_EDGES: bool = true;

    fn trace(&self, _: &mut Tracer<'_>) {}
}

impl<T> From<T> for Static<T> {
    fn from(t: T) -> Self {
        Self(t)
    }
}

impl<T: ?Sized> Deref for Static<T> {
    type Target = T;

    fn deref(&self) -> &T {
        &self.0
    }
}

impl<T: ?Sized> DerefMut for Static<T> {
    fn deref_mut(&mut self) -> &mut T {
        &mut self.0
    }
}

/// 没有边的类型：`leaf!(A, B<..>)` 实现 `Trace`（它们必须是 `Send + Sync`）。
#[macro_export]
macro_rules! leaf {
    ($($ty:ty),* $(,)?) => {$(
        // SAFETY: 没有 Gc
        unsafe impl $crate::Trace for $ty {
            const NO_EDGES: bool = true;
            fn trace(&self, _: &mut $crate::Tracer<'_>) {}
        }
    )*};
}

/// `Gc<GcCell<T>>` 上的借用：取出来的借用活满整个 `'gc`（同 gc-arena 的 `Gc<RefLock>`）。
pub trait CellExt<'gc, T> {
    fn borrow(self) -> CellRef<'gc, T>;
    fn try_borrow(self) -> Option<CellRef<'gc, T>>;
    fn borrow_mut(self, mc: &Mutation<'gc>) -> CellMut<'gc, T>;
    fn try_borrow_mut(self, mc: &Mutation<'gc>) -> Option<CellMut<'gc, T>>;
}

impl<'gc, T: Trace + 'gc> CellExt<'gc, T> for Gc<'gc, GcCell<'gc, T>> {
    #[inline]
    fn borrow(self) -> CellRef<'gc, T> {
        self.get().borrow()
    }

    #[inline]
    fn try_borrow(self) -> Option<CellRef<'gc, T>> {
        self.get().try_borrow()
    }

    #[inline]
    fn borrow_mut(self, mc: &Mutation<'gc>) -> CellMut<'gc, T> {
        self.get().borrow_mut(mc)
    }

    #[inline]
    fn try_borrow_mut(self, mc: &Mutation<'gc>) -> Option<CellMut<'gc, T>> {
        self.get().try_borrow_mut(mc)
    }
}

/// How a plain field type is kept when it must change in place in a heap object (`#[j8::heap]` with `#[mutable]`):
/// the cell type, and loading / storing through it (stores take the [`Mutation`] for the write barrier).
pub trait Mutable<'gc>: Sized {
    type Cell: Trace + 'gc;
    /// The cell holding `v` (before the object is on the heap: no barrier)
    fn cell(v: Self) -> Self::Cell;
    fn load(cell: &Self::Cell) -> Self;
    fn store(cell: &Self::Cell, mc: &Mutation<'gc>, v: Self);
}

impl<'gc> Mutable<'gc> for Value<'gc> {
    type Cell = ValueCell<'gc>;

    fn cell(v: Self) -> Self::Cell {
        ValueCell::new(v)
    }

    #[inline]
    fn load(cell: &Self::Cell) -> Self {
        cell.get()
    }

    #[inline]
    fn store(cell: &Self::Cell, mc: &Mutation<'gc>, v: Self) {
        cell.set(mc, v);
    }
}

impl<'gc, T: Trace + 'gc> Mutable<'gc> for Option<Gc<'gc, T>> {
    type Cell = Slot<'gc, T>;

    fn cell(v: Self) -> Self::Cell {
        Slot::from(v)
    }

    #[inline]
    fn load(cell: &Self::Cell) -> Self {
        cell.get()
    }

    #[inline]
    fn store(cell: &Self::Cell, mc: &Mutation<'gc>, v: Self) {
        cell.set(mc.mutator(), v);
    }
}

impl<'gc, T: Trace + 'gc> Mutable<'gc> for Gc<'gc, T> {
    type Cell = Slot<'gc, T>;

    fn cell(v: Self) -> Self::Cell {
        Slot::from(v)
    }

    #[inline]
    fn load(cell: &Self::Cell) -> Self {
        cell.get().expect("a non-optional Gc field is always set")
    }

    #[inline]
    fn store(cell: &Self::Cell, mc: &Mutation<'gc>, v: Self) {
        cell.set(mc.mutator(), Some(v));
    }
}

/// Scalars and `f64`: an [`IsoCell`] (no barrier)
macro_rules! mutable_scalar {
    ($($t:ty),* $(,)?) => {$(
        impl<'gc> Mutable<'gc> for $t {
            type Cell = IsoCell<$t>;

            fn cell(v: Self) -> Self::Cell {
                IsoCell::new(v)
            }

            #[inline]
            fn load(cell: &Self::Cell) -> Self {
                cell.get()
            }

            #[inline]
            fn store(cell: &Self::Cell, _: &Mutation<'gc>, v: Self) {
                cell.set(v);
            }
        }
    )*};
}

mutable_scalar!(bool, u8, u16, u32, u64, usize, i32, i64, f64);
