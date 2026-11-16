//! `J8Heap`: a collected heap plus its root, collected in parallel on a borrowed thread pool. Any number of threads
//! may `mutate` it at once (when the root is `Sync`); collection waits until none is inside. `Mutation`: one
//! thread's token inside `mutate` (its allocation buffer and write-barrier records); `Tlab`: a thread's allocation
//! buffer kept across `mutate` calls.
use alloc::boxed::Box;
use alloc::sync::Arc;
use alloc::vec::Vec;
use core::cell::{Cell, UnsafeCell};
use core::marker::PhantomData;
use core::mem::take;
use core::ptr::{self, NonNull};
use core::sync::atomic::{AtomicBool, AtomicUsize, Ordering};

use crate::context::{Context, Phase, marked_bit};
use crate::header::{Flags, Ptr, SizeClass, Typed, VTable};
use crate::parallel::Backoff;
use crate::{Collect, Metrics, Pool};

/// A thread's allocation buffer in the young generation, kept across `mutate` calls (a slice ends `mutate` often;
/// starting a fresh block each time would waste the rest of the old one). Invalidated by each minor collection.
#[derive(Default, Debug)]
pub struct Tlab {
    cursor: usize,
    end: usize,
    /// The young generation it was carved from
    generation: usize,
}

/// The token obtained inside `mutate`: required for allocation (`Gc::new`) and writes (`borrow_mut` / `set`).
/// Each mutating thread has its own: it bump-allocates in its own buffer and keeps its own write-barrier records,
/// handed to the collector when it leaves `mutate`. Not `Send` / `Sync`.
pub struct Mutation<'gc> {
    cx: NonNull<Context>,
    /// Copied when entering `mutate` (they only change during collection, while no mutator runs)
    marking: bool,
    epoch: bool,
    cursor: Cell<usize>,
    end: Cell<usize>,
    /// Objects the barrier grayed again during marking
    gray: UnsafeCell<Vec<Ptr>>,
    /// Old objects that may now point into the young generation
    remembered: UnsafeCell<Vec<Ptr>>,
    /// Young objects with drop glue
    drops: UnsafeCell<Vec<Ptr>>,
    _brand: PhantomData<Cell<&'gc ()>>,
}

impl Mutation<'_> {
    fn new(cx: &Context, tlab: &Tlab) -> Mutation<'static> {
        let current = tlab.generation == cx.generation();
        Mutation {
            cx: NonNull::from(cx),
            marking: cx.marking(),
            epoch: cx.epoch(),
            cursor: Cell::new(if current { tlab.cursor } else { 0 }),
            end: Cell::new(if current { tlab.end } else { 0 }),
            gray: UnsafeCell::default(),
            remembered: UnsafeCell::default(),
            drops: UnsafeCell::default(),
            _brand: PhantomData,
        }
    }

    /// # Safety
    /// The returned reference must not outlive `self`; the caller uses `'gc` as the brand.
    unsafe fn rebrand<'gc>(&self) -> &'gc Mutation<'gc> {
        // SAFETY: only the brand differs (a phantom)
        unsafe { &*ptr::from_ref(self).cast::<Mutation<'gc>>() }
    }

    /// Leaving `mutate`: hand the records in, keep the rest of the allocation buffer
    fn finish(&self, tlab: Option<&mut Tlab>) {
        // SAFETY: not reentrant; nothing else borrows the records now
        let (gray, remembered, drops) = unsafe { (take(&mut *self.gray.get()), take(&mut *self.remembered.get()), take(&mut *self.drops.get())) };
        self.cx().hand_in(gray, remembered, drops);
        if let Some(t) = tlab {
            *t = Tlab { cursor: self.cursor.get(), end: self.end.get(), generation: self.cx().generation() };
        }
    }

    pub(crate) fn cx(&self) -> &Context {
        // SAFETY: the heap outlives every `mutate` call, and no collection runs while a mutator is inside
        unsafe { self.cx.as_ref() }
    }

    pub fn metrics(&self) -> &Metrics {
        &self.cx().metrics
    }

    #[inline]
    fn is_marked(&self, f: u32) -> bool {
        (f & Flags::MARK != 0) == self.epoch
    }

    /// Mark bit for new objects: during marking they count as unmarked (reachable ones will get marked); otherwise as
    /// marked (they become unmarked naturally when the next cycle flips the epoch)
    #[inline]
    fn fresh(&self) -> u32 {
        if self.marking { marked_bit(self.epoch) ^ Flags::MARK } else { marked_bit(self.epoch) }
    }

    // ---- Allocation ----

    #[inline]
    pub(crate) fn alloc<'gc, T: Collect<'gc>>(&self, value: T) -> Ptr {
        let vt = &<T as Typed<'gc>>::VTABLE;
        let p = if vt.class == SizeClass::LARGE { self.alloc_large(vt) } else { self.bump(vt) };
        // SAFETY: a freshly allocated slot, sized and aligned for T
        unsafe { p.cast::<T>().write(value) };
        p
    }

    #[inline]
    fn bump(&self, vt: &VTable) -> Ptr {
        let mut at = self.cursor.get();
        if self.end.get() - at < vt.slot {
            at = self.refill();
        }
        self.cursor.set(at + vt.slot);
        let p = Ptr::at_slot(NonNull::new(at as *mut u8).expect("nursery slot"));
        // SAFETY: a slot newly carved out of this thread's buffer
        unsafe { p.init(vt, self.fresh()) };
        if vt.drop.is_some() {
            // SAFETY: not reentrant
            unsafe { (*self.drops.get()).push(p) };
        }
        p
    }

    #[cold]
    fn refill(&self) -> usize {
        let (start, end) = self.cx().refill();
        self.end.set(end);
        start
    }

    /// Large objects go straight into the old generation; their fields may point into the young generation, so they
    /// start in the remembered set
    #[cold]
    fn alloc_large(&self, vt: &VTable) -> Ptr {
        let p = self.cx().alloc_large(vt);
        // SAFETY: a large object's header precedes the value and belongs to this allocation
        unsafe { p.init(vt, Flags::OLD | Flags::LARGE | Flags::REMEMBERED | self.fresh()) };
        // SAFETY: not reentrant
        unsafe { (*self.remembered.get()).push(p) };
        p
    }

    // ---- Write barrier ----

    /// Called before object `p` receives a new pointer: old objects go into the remembered set; black objects turn
    /// gray again during marking. Several threads may hit the same object: whoever turns the bit on records it
    #[inline]
    pub(crate) fn barrier(&self, p: Ptr) {
        let f = p.header().flags();
        if f & (Flags::OLD | Flags::REMEMBERED) == Flags::OLD {
            self.remember(p);
        }
        if self.marking && f & Flags::GRAY == 0 && self.is_marked(f) {
            self.gray_again(p);
        }
    }

    #[cold]
    fn remember(&self, p: Ptr) {
        if p.header().or_flags(Flags::REMEMBERED) & Flags::REMEMBERED == 0 {
            // SAFETY: not reentrant
            unsafe { (*self.remembered.get()).push(p) };
        }
    }

    #[cold]
    fn gray_again(&self, p: Ptr) {
        if p.header().or_flags(Flags::GRAY) & Flags::GRAY == 0 {
            // SAFETY: not reentrant
            unsafe { (*self.gray.get()).push(p) };
        }
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

/// The safepoint gate: how many mutators are inside `mutate`, plus a bit for "a collection wants in". Mutators never
/// wait for each other; they wait only while a collection is requested, and the collection waits for them to leave.
#[derive(Default)]
struct Gate(AtomicUsize);

impl Gate {
    /// The top bit: a collection is requested or running
    const REQUEST: usize = 1 << (usize::BITS - 1);

    fn enter(&self) {
        let mut backoff = Backoff::default();
        loop {
            let s = self.0.load(Ordering::Acquire);
            if s & Self::REQUEST == 0 && self.0.compare_exchange_weak(s, s + 1, Ordering::AcqRel, Ordering::Acquire).is_ok() {
                return;
            }
            backoff.wait();
        }
    }

    fn leave(&self) {
        self.0.fetch_sub(1, Ordering::AcqRel);
    }

    /// Requests a collection and waits until no mutator is inside; reopens when the guard drops
    fn close(&self) -> Closed<'_> {
        let mut backoff = Backoff::default();
        while self.0.fetch_or(Self::REQUEST, Ordering::AcqRel) & Self::REQUEST != 0 {
            backoff.wait();
        }
        while self.0.load(Ordering::Acquire) != Self::REQUEST {
            backoff.wait();
        }
        Closed(self)
    }
}

/// A closed gate: no mutator is inside until it drops.
struct Closed<'g>(&'g Gate);

impl Drop for Closed<'_> {
    fn drop(&mut self) {
        self.0.0.fetch_and(!Gate::REQUEST, Ordering::Release);
    }
}

/// The heap's own allocation buffer, lent to whichever `mutate` call takes it first (the common single-threaded
/// case keeps its buffer across calls without the caller holding a [`Tlab`]).
#[derive(Default)]
struct Primary {
    busy: AtomicBool,
    tlab: UnsafeCell<Tlab>,
}

// SAFETY: the buffer is touched only by whoever set `busy`
unsafe impl Sync for Primary {}

/// Where a `mutate` call keeps its allocation buffer.
enum Lent<'h> {
    /// The heap's own (taken)
    Primary(&'h Primary),
    /// The caller's
    Own(&'h mut Tlab),
    /// None: the rest of the block is left unused when the call ends
    Fresh,
}

/// Inside `mutate`: the mutator's token; leaving (even by unwinding) hands its records in and leaves the gate.
struct Entered<'h> {
    gate: &'h Gate,
    m: Mutation<'static>,
    tlab: Lent<'h>,
}

impl<'h> Entered<'h> {
    /// The caller has entered `gate` (only then may it look at the context)
    fn new(gate: &'h Gate, cx: &Context, tlab: Lent<'h>) -> Self {
        let m = match &tlab {
            // SAFETY: taken via `busy`
            Lent::Primary(p) => Mutation::new(cx, unsafe { &*p.tlab.get() }),
            Lent::Own(t) => Mutation::new(cx, t),
            Lent::Fresh => Mutation::new(cx, &Tlab::default()),
        };
        Self { gate, m, tlab }
    }
}

impl Drop for Entered<'_> {
    fn drop(&mut self) {
        match &mut self.tlab {
            Lent::Primary(p) => {
                // SAFETY: still taken via `busy`
                self.m.finish(Some(unsafe { &mut *p.tlab.get() }));
                p.busy.store(false, Ordering::Release);
            }
            Lent::Own(t) => self.m.finish(Some(t)),
            Lent::Fresh => self.m.finish(None),
        }
        self.gate.leave();
    }
}

/// A collected heap: its own young generation, old generation and root; collection runs in parallel on `pool`.
///
/// Each [`J8Heap::mutate`] is a fresh `'gc` brand: a `Gc` cannot escape the call or enter another heap, so several
/// heaps (which may share a pool) stay independent. Several threads may `mutate` at once when the root is `Sync`
/// (a `Gc` is `Send` / `Sync` only when its target is `Sync`). Collection only happens while no thread is inside
/// `mutate` (exclusively through `&mut self`, or behind the safepoint gate through `&self`), when every `Gc` is
/// reachable from the root, so objects can move.
pub struct J8Heap<'x, R>
where
    R: for<'a> Rootable<'a>,
    for<'a> Root<'a, R>: Sized,
{
    pool: &'x dyn Pool,
    /// Mutators read it through `&` while inside `mutate`; collection takes it `&mut` while none is. Never
    /// referenced outside the gate except through `&mut self`
    cx: Box<UnsafeCell<Context>>,
    /// The context's counters, readable at any time
    metrics: Arc<Metrics>,
    /// `Box<Root<_, R>>` with the brand erased
    root: NonNull<u8>,
    gate: Gate,
    primary: Primary,
    _root: PhantomData<R>,
}

// SAFETY: the context is shared only through atomics / lock-free structures while mutators run, and taken exclusively
// only behind the gate; the root is shared as `&Root` and so must be `Sync`
unsafe impl<R> Sync for J8Heap<'_, R>
where
    R: for<'a> Rootable<'a>,
    for<'a> Root<'a, R>: Sized + Sync,
{
}

// SAFETY: the heap owns its context and root
unsafe impl<R> Send for J8Heap<'_, R>
where
    R: for<'a> Rootable<'a>,
    for<'a> Root<'a, R>: Sized + Send,
{
}

impl<'x, R> J8Heap<'x, R>
where
    R: for<'a> Rootable<'a>,
    for<'a> Root<'a, R>: Sized + Collect<'a>,
{
    /// Creates a heap: collection uses `pool` (moremath's `ThreadMulti` / `ThreadSingle` or any [`Pool`]); `f` builds
    /// the root from a `&Mutation`
    pub fn new(pool: &'x dyn Pool, f: impl for<'gc> FnOnce(&'gc Mutation<'gc>) -> Root<'gc, R>) -> Self {
        let cx = Box::new(UnsafeCell::new(Context::new()));
        // SAFETY: nobody else sees the context yet
        let m = Mutation::new(unsafe { &*cx.get() }, &Tlab::default());
        // SAFETY: the root lives as long as the heap; the brand is confined to f
        let root = f(unsafe { m.rebrand() });
        m.finish(None);
        // SAFETY: as above
        let metrics = unsafe { &*cx.get() }.metrics.clone();
        Self { pool, root: NonNull::from(Box::leak(Box::new(root))).cast(), cx, metrics, gate: Gate::default(), primary: Primary::default(), _root: PhantomData }
    }

    /// The thread pool used for collection
    pub fn pool(&self) -> &'x dyn Pool {
        self.pool
    }

    /// # Safety
    /// Only while inside the gate (no collection can take the context meanwhile).
    fn cx(&self) -> &Context {
        // SAFETY: the callers are inside the gate; exclusive access happens only through &mut self or behind the
        // closed gate
        unsafe { &*self.cx.get() }
    }

    /// Reads something off the context from inside the gate
    fn peek<T>(&self, f: impl FnOnce(&Context) -> T) -> T {
        self.gate.enter();
        let r = f(self.cx());
        self.gate.leave();
        r
    }

    /// # Safety
    /// The returned reference must not outlive the next collection, and must be the only reference to the root.
    unsafe fn root<'gc>(&self) -> &'gc mut Root<'gc, R> {
        unsafe { self.root.cast::<Root<'gc, R>>().as_mut() }
    }

    /// # Safety
    /// The returned reference must not outlive the next collection (shared: several mutators hold it at once).
    unsafe fn root_ref<'gc>(&self) -> &'gc Root<'gc, R> {
        unsafe { self.root.cast::<Root<'gc, R>>().as_ref() }
    }

    fn enter<'h>(&'h self, tlab: Option<&'h mut Tlab>) -> Entered<'h> {
        let lent = match tlab {
            Some(t) => Lent::Own(t),
            None if self.primary.busy.compare_exchange(false, true, Ordering::AcqRel, Ordering::Acquire).is_ok() => Lent::Primary(&self.primary),
            None => Lent::Fresh,
        };
        // Enter first: until then a collection may hold the context exclusively
        self.gate.enter();
        Entered::new(&self.gate, self.cx(), lent)
    }

    /// Reads and writes the heap: no collection happens during `f`, and a `Gc` cannot escape `f`. Any number of
    /// threads may be inside at once.
    pub fn mutate<T>(&self, f: impl for<'gc> FnOnce(&'gc Mutation<'gc>, &'gc Root<'gc, R>) -> T) -> T {
        let e = self.enter(None);
        // SAFETY: no collection while inside the gate; the brand prevents references from escaping
        f(unsafe { e.m.rebrand() }, unsafe { self.root_ref() })
    }

    /// Like [`J8Heap::mutate`], allocating in the caller's own buffer (a thread that mutates repeatedly keeps one)
    pub fn mutate_in<T>(&self, tlab: &mut Tlab, f: impl for<'gc> FnOnce(&'gc Mutation<'gc>, &'gc Root<'gc, R>) -> T) -> T {
        let e = self.enter(Some(tlab));
        // SAFETY: as above
        f(unsafe { e.m.rebrand() }, unsafe { self.root_ref() })
    }

    /// Like [`J8Heap::mutate`], but the root is mutable (the root is fully rescanned on every collection, so no write
    /// barrier is needed)
    pub fn mutate_root<T>(&mut self, f: impl for<'gc> FnOnce(&'gc Mutation<'gc>, &'gc mut Root<'gc, R>) -> T) -> T {
        let e = self.enter(None);
        // SAFETY: as above; &mut self guarantees the root is not shared
        f(unsafe { e.m.rebrand() }, unsafe { self.root() })
    }

    /// Collects a step according to the allocation debt: a minor collection when the young generation is full, and
    /// advancing the incremental major collection once the old generation passes its threshold
    pub fn collect_debt(&mut self) {
        // SAFETY: &mut self: no mutator is inside, the root is the only entry point
        let root = unsafe { self.root() };
        self.cx.get_mut().collect_debt(self.pool, root);
    }

    /// One full collection cycle: minor collection + mark-sweep + compaction when worthwhile
    pub fn collect_all(&mut self) {
        // SAFETY: as above
        let root = unsafe { self.root() };
        self.cx.get_mut().collect_all(self.pool, root);
    }

    /// [`J8Heap::collect_debt`] through a shared heap: waits until no thread is inside `mutate` (new ones wait for
    /// it). Must not be called from inside `mutate` (it would wait for itself)
    pub fn collect_debt_shared(&self) {
        let _closed = self.gate.close();
        // SAFETY: the gate is closed: no mutator is inside, nobody else collects
        unsafe { (*self.cx.get()).collect_debt(self.pool, self.root()) };
    }

    /// [`J8Heap::collect_all`] through a shared heap (same rules as [`J8Heap::collect_debt_shared`])
    pub fn collect_all_shared(&self) {
        let _closed = self.gate.close();
        // SAFETY: as above
        unsafe { (*self.cx.get()).collect_all(self.pool, self.root()) };
    }

    pub fn metrics(&self) -> &Metrics {
        &self.metrics
    }

    /// Drops dead objects (of types with `Collect::PARALLEL_DROP`) in parallel on the thread pool. Off by default:
    /// with glibc's malloc, freeing across threads contends for the original thread's arena lock, so parallel is
    /// actually slower; turn it on with an allocator like jemalloc / mimalloc.
    pub fn set_parallel_drop(&mut self, on: bool) {
        self.cx.get_mut().parallel_drop = on;
    }

    /// How far the major collection has progressed
    pub fn phase(&self) -> Phase {
        self.peek(Context::phase)
    }

    /// Bytes handed to the young generation's allocation buffers
    pub fn young_bytes(&self) -> usize {
        self.peek(Context::young_bytes)
    }

    /// Bytes occupied by the old generation
    pub fn old_bytes(&self) -> usize {
        self.peek(Context::old_bytes)
    }
}

impl<R> Drop for J8Heap<'_, R>
where
    R: for<'a> Rootable<'a>,
    for<'a> Root<'a, R>: Sized,
{
    fn drop(&mut self) {
        self.drop_root();
    }
}

impl<R> J8Heap<'_, R>
where
    R: for<'a> Rootable<'a>,
    for<'a> Root<'a, R>: Sized,
{
    fn drop_root(&mut self) {
        // SAFETY: leaked via Box::leak in new and returned only once; the heap is torn down right after
        drop(unsafe { Box::from_raw(self.root.cast::<Root<'_, R>>().as_ptr()) });
    }
}
