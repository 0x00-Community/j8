//! `R8Heap`: a collected heap plus its root, collected in parallel on a borrowed thread pool; `Mutation`: the token
//! for allocation and write barriers inside `mutate`.
use alloc::boxed::Box;
use core::cell::Cell;
use core::marker::PhantomData;
use core::ptr::NonNull;

use crate::context::{Context, Phase};
use crate::{Collect, Metrics, Pool};

/// The token obtained inside `mutate`: required for allocation (`Gc::new`) and writes (`borrow_mut` / `set`).
#[repr(transparent)]
pub struct Mutation<'gc> {
    cx: Context,
    _brand: PhantomData<Cell<&'gc ()>>,
}

impl Mutation<'_> {
    /// # Safety
    /// The returned reference must not outlive `cx`; the caller uses `'gc` as the brand.
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

/// Produces the root type for any lifetime `'a`. Write it with [`Rootable!`](crate::Rootable!).
pub trait Rootable<'a> {
    type Root: ?Sized + 'a;
}

/// Marker type used by `Rootable!` (instead of a bare trait object, avoiding the `?Sized` bound).
#[doc(hidden)]
pub struct __DynRootable<T: ?Sized>(PhantomData<T>);

impl<'a, T: ?Sized + Rootable<'a>> Rootable<'a> for __DynRootable<T> {
    type Root = <T as Rootable<'a>>::Root;
}

/// The root type at lifetime `'a`.
pub type Root<'a, R> = <R as Rootable<'a>>::Root;

/// Writes a root type: `Rootable![MyRoot<'_>]` (`'_` is replaced by the brand), or `Rootable!['gc => MyRoot<'gc>]`.
#[macro_export]
macro_rules! Rootable {
    ($gc:lifetime => $root:ty) => {
        $crate::__DynRootable::<dyn for<$gc> $crate::Rootable<$gc, Root = $root>>
    };
    ($root:ty) => {
        $crate::Rootable!['__gc => $crate::__unelide_lifetimes!('__gc; $root)]
    };
}

/// A collected heap: its own young generation, old generation and root; collection runs in parallel on `pool`.
///
/// Each [`R8Heap::mutate`] is a fresh `'gc` brand: a `Gc` cannot escape the call or enter another heap, so several
/// heaps (which may share a pool) stay independent. Collection only happens outside `mutate`
/// ([`R8Heap::collect_debt`] / [`R8Heap::collect_all`]), when every `Gc` is reachable from the root, so objects can
/// move.
pub struct R8Heap<'x, R>
where
    R: for<'a> Rootable<'a>,
    for<'a> Root<'a, R>: Sized,
{
    pool: &'x dyn Pool,
    cx: Box<Context>,
    /// `Box<Root<_, R>>` with the brand erased
    root: NonNull<u8>,
    _root: PhantomData<R>,
}

impl<'x, R> R8Heap<'x, R>
where
    R: for<'a> Rootable<'a>,
    for<'a> Root<'a, R>: Sized + Collect<'a>,
{
    /// Creates a heap: collection uses `pool` (moremath's `ThreadMulti` / `ThreadSingle` or any [`Pool`]); `f` builds
    /// the root from a `&Mutation`
    pub fn new(pool: &'x dyn Pool, f: impl for<'gc> FnOnce(&'gc Mutation<'gc>) -> Root<'gc, R>) -> Self {
        let cx = Box::new(Context::new());
        // SAFETY: the root lives as long as the heap; the brand is confined to f
        let root = f(unsafe { Mutation::of(&cx) });
        Self { pool, root: NonNull::from(Box::leak(Box::new(root))).cast(), cx, _root: PhantomData }
    }

    /// The thread pool used for collection
    pub fn pool(&self) -> &'x dyn Pool {
        self.pool
    }

    /// # Safety
    /// The returned references must not outlive the next collection.
    unsafe fn parts<'gc>(&self) -> (&'gc Mutation<'gc>, &'gc mut Root<'gc, R>) {
        unsafe { (Mutation::of(&self.cx), self.root.cast::<Root<'gc, R>>().as_mut()) }
    }

    /// Reads and writes the heap: no collection happens during `f`, and a `Gc` cannot escape `f`
    pub fn mutate<T>(&self, f: impl for<'gc> FnOnce(&'gc Mutation<'gc>, &'gc Root<'gc, R>) -> T) -> T {
        // SAFETY: no collection during f; the brand prevents references from escaping
        let (mc, root) = unsafe { self.parts() };
        f(mc, root)
    }

    /// Like [`R8Heap::mutate`], but the root is mutable (the root is fully rescanned on every collection, so no write
    /// barrier is needed)
    pub fn mutate_root<T>(&mut self, f: impl for<'gc> FnOnce(&'gc Mutation<'gc>, &'gc mut Root<'gc, R>) -> T) -> T {
        // SAFETY: as above; &mut self guarantees exclusive access
        let (mc, root) = unsafe { self.parts() };
        f(mc, root)
    }

    /// Collects a step according to the allocation debt: a minor collection when the young generation is full, and
    /// advancing the incremental major collection once the old generation passes its threshold
    pub fn collect_debt(&mut self) {
        // SAFETY: not inside mutate, so the root is the only entry point
        let (_, root) = unsafe { self.parts() };
        self.cx.collect_debt(self.pool, root);
    }

    /// One full collection cycle: minor collection + mark-sweep + compaction when worthwhile
    pub fn collect_all(&mut self) {
        // SAFETY: as above
        let (_, root) = unsafe { self.parts() };
        self.cx.collect_all(self.pool, root);
    }

    pub fn metrics(&self) -> &Metrics {
        &self.cx.metrics
    }

    /// Drops dead objects (of types with `Collect::PARALLEL_DROP`) in parallel on the thread pool. Off by default:
    /// with glibc's malloc, freeing across threads contends for the original thread's arena lock, so parallel is
    /// actually slower; turn it on with an allocator like jemalloc / mimalloc.
    pub fn set_parallel_drop(&mut self, on: bool) {
        self.cx.parallel_drop = on;
    }

    /// How far the major collection has progressed
    pub fn phase(&self) -> Phase {
        self.cx.phase()
    }

    /// Bytes used by the young generation
    pub fn young_bytes(&self) -> usize {
        self.cx.young_bytes()
    }

    /// Bytes occupied by the old generation
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
        // SAFETY: leaked via Box::leak in new and returned only once; the heap is torn down right after
        drop(unsafe { Box::from_raw(self.root.cast::<Root<'_, R>>().as_ptr()) });
    }
}
