//! `R8Heap`：一个回收堆加它的根，回收在借来的线程池上并行；`Mutation`：`mutate` 里分配与写屏障的凭据。
use alloc::boxed::Box;
use core::cell::Cell;
use core::marker::PhantomData;
use core::ptr::NonNull;

use crate::context::{Context, Phase};
use crate::{Collect, Metrics, Pool};

/// `mutate` 里拿到的凭据：分配（`Gc::new`）、写（`borrow_mut` / `set`）都要它。
#[repr(transparent)]
pub struct Mutation<'gc> {
    cx: Context,
    _brand: PhantomData<Cell<&'gc ()>>,
}

impl Mutation<'_> {
    /// # Safety
    /// 得到的引用活不过 `cx`；`'gc` 由调用方当品牌用。
    unsafe fn of<'a>(cx: &Context) -> &'a Self {
        // SAFETY: repr(transparent)
        unsafe { &*core::ptr::from_ref(cx).cast::<Self>() }
    }

    pub(crate) fn cx(&self) -> &Context {
        &self.cx
    }

    pub fn metrics(&self) -> &Metrics {
        &self.cx.metrics
    }
}

/// 给任意生命周期 `'a` 产出根类型。用 [`Rootable!`](crate::Rootable!) 写。
pub trait Rootable<'a> {
    type Root: ?Sized + 'a;
}

/// `Rootable!` 用的标记类型（代替裸 trait 对象，省去 `?Sized` 约束）。
#[doc(hidden)]
pub struct __DynRootable<T: ?Sized>(PhantomData<T>);

impl<'a, T: ?Sized + Rootable<'a>> Rootable<'a> for __DynRootable<T> {
    type Root = <T as Rootable<'a>>::Root;
}

/// 根类型在 `'a` 下的样子。
pub type Root<'a, R> = <R as Rootable<'a>>::Root;

/// 写根类型：`Rootable![MyRoot<'_>]`（`'_` 换成品牌），或 `Rootable!['gc => MyRoot<'gc>]`。
#[macro_export]
macro_rules! Rootable {
    ($gc:lifetime => $root:ty) => {
        $crate::__DynRootable::<dyn for<$gc> $crate::Rootable<$gc, Root = $root>>
    };
    ($root:ty) => {
        $crate::Rootable!['__gc => $crate::__unelide_lifetimes!('__gc; $root)]
    };
}

/// 一个回收堆：自己的新生代、老年代与根，回收借 `pool` 并行做。
///
/// 每次 [`R8Heap::mutate`] 都是一个新的 `'gc` 品牌：`Gc` 出不了这次调用，也进不了别的堆，
/// 所以几个堆（可以共用一个池）各管各的。回收只在 `mutate` 之外发生
/// （[`R8Heap::collect_debt`] / [`R8Heap::collect_all`]），那时所有 `Gc` 都在根下面，对象可以搬。
pub struct R8Heap<'x, R>
where
    R: for<'a> Rootable<'a>,
    for<'a> Root<'a, R>: Sized,
{
    pool: &'x dyn Pool,
    cx: Box<Context>,
    /// `Box<Root<_, R>>`，擦掉了品牌
    root: NonNull<u8>,
    _root: PhantomData<R>,
}

impl<'x, R> R8Heap<'x, R>
where
    R: for<'a> Rootable<'a>,
    for<'a> Root<'a, R>: Sized + Collect<'a>,
{
    /// 建堆：回收用 `pool`（moremath 的 `ThreadMulti` / `ThreadSingle` 或任何 [`Pool`]）；`f` 拿 `&Mutation` 造根
    pub fn new(pool: &'x dyn Pool, f: impl for<'gc> FnOnce(&'gc Mutation<'gc>) -> Root<'gc, R>) -> Self {
        let cx = Box::new(Context::new());
        // SAFETY: 根随堆一起活，品牌只在 f 里
        let root = f(unsafe { Mutation::of(&cx) });
        Self { pool, root: NonNull::from(Box::leak(Box::new(root))).cast(), cx, _root: PhantomData }
    }

    /// 回收用的线程池
    pub fn pool(&self) -> &'x dyn Pool {
        self.pool
    }

    /// # Safety
    /// 返回的引用不能活过下一次回收。
    unsafe fn parts<'gc>(&self) -> (&'gc Mutation<'gc>, &'gc mut Root<'gc, R>) {
        unsafe { (Mutation::of(&self.cx), self.root.cast::<Root<'gc, R>>().as_mut()) }
    }

    /// 读写堆：`f` 期间不回收，`Gc` 出不了 `f`
    pub fn mutate<T>(&self, f: impl for<'gc> FnOnce(&'gc Mutation<'gc>, &'gc Root<'gc, R>) -> T) -> T {
        // SAFETY: f 期间没有回收；品牌挡住引用外逃
        let (mc, root) = unsafe { self.parts() };
        f(mc, root)
    }

    /// 同 [`R8Heap::mutate`]，根可改（根每次回收都整个重扫，不用写屏障）
    pub fn mutate_root<T>(&mut self, f: impl for<'gc> FnOnce(&'gc Mutation<'gc>, &'gc mut Root<'gc, R>) -> T) -> T {
        // SAFETY: 同上；&mut self 保证独占
        let (mc, root) = unsafe { self.parts() };
        f(mc, root)
    }

    /// 按欠账回收一段：新生代满了做次回收，老年代过阈值推进增量主回收
    pub fn collect_debt(&mut self) {
        // SAFETY: 不在 mutate 里，根是唯一入口
        let (_, root) = unsafe { self.parts() };
        self.cx.collect_debt(self.pool, root);
    }

    /// 完整回收一轮：次回收 + 标记清扫 + 值得就压缩
    pub fn collect_all(&mut self) {
        // SAFETY: 同上
        let (_, root) = unsafe { self.parts() };
        self.cx.collect_all(self.pool, root);
    }

    pub fn metrics(&self) -> &Metrics {
        &self.cx.metrics
    }

    /// 死对象（类型 `Collect::PARALLEL_DROP` 的）在线程池上并行析构。缺省关：
    /// glibc 的 malloc 跨线程释放要抢原线程的 arena 锁，并行反而更慢；换 jemalloc / mimalloc 这类再开。
    pub fn set_parallel_drop(&mut self, on: bool) {
        self.cx.parallel_drop = on;
    }

    /// 主回收进行到哪了
    pub fn phase(&self) -> Phase {
        self.cx.phase()
    }

    /// 新生代用了多少字节
    pub fn young_bytes(&self) -> usize {
        self.cx.young_bytes()
    }

    /// 老年代占了多少字节
    pub fn old_bytes(&self) -> usize {
        self.cx.old_bytes()
    }
}

impl<R> Drop for R8Heap<'_, R>
where
    R: for<'a> Rootable<'a>,
    for<'a> Root<'a, R>: Sized,
{
    fn drop(&mut self) {
        self.drop_root();
    }
}

impl<R> R8Heap<'_, R>
where
    R: for<'a> Rootable<'a>,
    for<'a> Root<'a, R>: Sized,
{
    fn drop_root(&mut self) {
        // SAFETY: new 里 Box::leak 出来的，只还一次；堆随后整个拆掉
        drop(unsafe { Box::from_raw(self.root.cast::<Root<'_, R>>().as_ptr()) });
    }
}
