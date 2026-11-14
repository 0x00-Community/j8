//! The collector: the mutator side (allocation, write barrier; single-threaded) and the collection phases (marking,
//! minor collection, sweeping, compaction; parallel on the thread pool).
use alloc::vec::Vec;
use core::cell::{Cell, RefCell};
use core::mem::take;
use core::ptr::NonNull;

use crate::collect::{Mover, Visit};
use crate::header::{Claim, Flags, Ptr, SizeClass, Typed, VTable};
use crate::heap::{Lab, NURSERY_LIMIT, Nursery, OldSpace};
use crate::parallel::{Pool, Spin, Work, Worker, split};
use crate::{Collect, Metrics, Relocate, Trace};

/// The old generation must grow at least this large before the first major collection starts
const MIN_HEAP: usize = 8 << 20;
/// After a major collection, the next one starts when the old generation grows to this multiple of the live objects
const GROWTH: usize = 2;
/// Each incremental marking step does at least this many bytes
const MARK_STEP: usize = 256 << 10;
/// Each incremental marking step does this multiple of what the old generation grew in the meantime
const MARK_RATIO: usize = 2;

/// How far the major collection has progressed.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Phase {
    /// No major collection in progress
    Sleep,
    /// Incremental marking in progress
    Mark,
}

/// The mark bit meaning "marked" under epoch `epoch`
fn marked_bit(epoch: bool) -> u32 {
    if epoch { Flags::MARK } else { 0 }
}

/// The marking worker: claims white objects and pushes them; traces them when popped, turning them black.
struct Marker {
    epoch: bool,
    stack: Vec<Ptr>,
}

impl Marker {
    fn new(epoch: bool) -> Self {
        Self { epoch, stack: Vec::new() }
    }
}

impl Visit for Marker {
    /// Claims a white object as gray and pushes it (objects that need no tracing turn black directly); skips it if
    /// another thread claimed it first
    #[inline]
    fn visit(&mut self, p: Ptr) {
        let needs = p.vtable().needs_trace;
        let epoch = self.epoch;
        let claimed = p.header().update(|f| {
            if f & Flags::GRAY != 0 || (f & Flags::MARK != 0) == epoch {
                None
            } else if needs {
                Some(f | Flags::GRAY)
            } else {
                Some(f ^ Flags::MARK)
            }
        });
        if claimed && needs {
            self.stack.push(p);
        }
    }
}

impl Worker for Marker {
    fn stack(&mut self) -> &mut Vec<Ptr> {
        &mut self.stack
    }

    fn process(&mut self, p: Ptr) -> usize {
        let marked = marked_bit(self.epoch);
        p.header().update(|f| Some(f & !(Flags::GRAY | Flags::MARK) | marked));
        // SAFETY: a gray object belongs to the thread that claimed it; the mutator is stopped
        unsafe { p.trace(&mut Trace::new(self)) };
        p.vtable().size
    }
}

/// The minor-collection worker: moves young objects into the old generation and updates the fields of moved objects.
struct Promoter<'a> {
    lab: Lab<'a>,
    epoch: bool,
    /// The major collection is marking: moved objects must go into the gray queue
    marking: bool,
    /// Moved objects whose fields have not been updated yet
    stack: Vec<Ptr>,
    gray: Vec<Ptr>,
    promoted: usize,
}

impl<'a> Promoter<'a> {
    fn new(old: &'a Spin<OldSpace>, epoch: bool, marking: bool) -> Self {
        Self { lab: Lab::new(old), epoch, marking, stack: Vec::new(), gray: Vec::new(), promoted: 0 }
    }
}

impl Mover for Promoter<'_> {
    fn young_only(&self) -> bool {
        true
    }

    /// Moves a young object into the old generation (if another thread already moved it, uses that copy)
    fn forward(&mut self, p: Ptr) -> Ptr {
        let h = p.header();
        match h.claim() {
            Claim::Old => p,
            Claim::Forwarded(q) => q,
            Claim::Mine => {
                let vt = p.vtable();
                let q = self.lab.slot(vt.class);
                // Objects moved during marking survive this cycle: those needing tracing go into the gray queue,
                // the rest become black directly
                let gray = self.marking && vt.needs_trace;
                let mark = if gray { Flags::GRAY | (marked_bit(self.epoch) ^ Flags::MARK) } else { marked_bit(self.epoch) };
                // SAFETY: a free slot we own; p is held exclusively through the BUSY bit
                unsafe { p.copy_to(vt, q, Flags::OLD | mark) };
                h.forward(q);
                if gray {
                    self.gray.push(q);
                }
                if vt.needs_trace {
                    self.stack.push(q);
                }
                self.promoted += 1;
                q
            }
        }
    }
}

impl Worker for Promoter<'_> {
    fn stack(&mut self) -> &mut Vec<Ptr> {
        &mut self.stack
    }

    fn process(&mut self, p: Ptr) -> usize {
        // SAFETY: a just-moved object or an old object from the remembered set, each owned by one thread
        unsafe { p.relocate(&mut Relocate::new(self)) };
        p.vtable().size
    }
}

/// Updates fields during compaction: pointers to moved objects are replaced by the new addresses.
struct Compactor;

impl Mover for Compactor {
    fn forward(&mut self, p: Ptr) -> Ptr {
        p.header().forwarded().unwrap_or(p)
    }
}

pub(crate) struct Context {
    phase: Cell<Phase>,
    /// The mark bit counts as marked only when equal to this
    epoch: Cell<bool>,
    /// Bump region of the young generation's current block
    cursor: Cell<usize>,
    end: Cell<usize>,
    nursery: RefCell<Nursery>,
    old: Spin<OldSpace>,
    gray: RefCell<Vec<Ptr>>,
    /// Old objects that may point into the young generation
    remembered: RefCell<Vec<Ptr>>,
    /// A major collection starts when the old generation grows this large
    threshold: Cell<usize>,
    /// Size of the old generation at the previous marking step
    marked_at: Cell<usize>,
    /// Dead objects of eligible types are dropped on the thread pool (worthwhile only when the allocator frees fast
    /// across threads)
    pub(crate) parallel_drop: bool,
    pub(crate) metrics: Metrics,
}

impl Context {
    pub(crate) fn new() -> Self {
        Self {
            phase: Cell::new(Phase::Sleep),
            epoch: Cell::new(false),
            cursor: Cell::new(0),
            end: Cell::new(0),
            nursery: RefCell::default(),
            old: Spin::default(),
            gray: RefCell::default(),
            remembered: RefCell::default(),
            threshold: Cell::new(MIN_HEAP),
            marked_at: Cell::new(0),
            parallel_drop: false,
            metrics: Metrics::default(),
        }
    }

    pub(crate) fn phase(&self) -> Phase {
        self.phase.get()
    }

    pub(crate) fn young_bytes(&self) -> usize {
        self.nursery.borrow().used(self.cursor.get())
    }

    pub(crate) fn old_bytes(&self) -> usize {
        self.old.lock().bytes
    }

    #[inline]
    fn is_marked(&self, f: u32) -> bool {
        (f & Flags::MARK != 0) == self.epoch.get()
    }

    /// Mark bit for new objects: during marking they count as unmarked (reachable ones will get marked); otherwise as
    /// marked (they become unmarked naturally when the next cycle flips the epoch)
    #[inline]
    fn fresh(&self) -> u32 {
        match self.phase.get() {
            Phase::Mark => marked_bit(self.epoch.get()) ^ Flags::MARK,
            Phase::Sleep => marked_bit(self.epoch.get()),
        }
    }

    // ---- Allocation (mutator) ----

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
        // SAFETY: a slot newly carved out of the bump region
        unsafe { p.init(vt, self.fresh()) };
        if vt.drop.is_some() {
            self.nursery.borrow_mut().drops.push(p);
        }
        p
    }

    #[cold]
    fn refill(&self) -> usize {
        let (start, end) = self.nursery.borrow_mut().refill();
        self.end.set(end);
        start
    }

    /// Large objects go straight into the old generation; their fields may point into the young generation, so they
    /// start in the remembered set
    #[cold]
    fn alloc_large(&self, vt: &VTable) -> Ptr {
        let p = self.old.lock().alloc_large(vt);
        // SAFETY: a large object's header precedes the value and belongs to this allocation
        unsafe { p.init(vt, Flags::OLD | Flags::LARGE | Flags::REMEMBERED | self.fresh()) };
        self.remembered.borrow_mut().push(p);
        p
    }

    // ---- Write barrier (mutator) ----

    /// Called before object `p` receives a new pointer: old objects go into the remembered set; black objects turn
    /// gray again during marking
    #[inline]
    pub(crate) fn barrier(&self, p: Ptr) {
        let f = p.header().flags();
        if f & (Flags::OLD | Flags::REMEMBERED) == Flags::OLD {
            self.remember(p);
        }
        if self.phase.get() == Phase::Mark && f & Flags::GRAY == 0 && self.is_marked(f) {
            self.gray_again(p);
        }
    }

    #[cold]
    fn remember(&self, p: Ptr) {
        p.header().or_flags(Flags::REMEMBERED);
        self.remembered.borrow_mut().push(p);
    }

    #[cold]
    fn gray_again(&self, p: Ptr) {
        p.header().or_flags(Flags::GRAY);
        self.gray.borrow_mut().push(p);
    }

    // ---- Marking ----

    /// Parallel marking starting from `initial`; with a `budget`, stops after that many bytes and returns the
    /// unfinished work
    fn mark(&self, pool: &dyn Pool, initial: Vec<Ptr>, budget: Option<usize>) -> Vec<Ptr> {
        if initial.is_empty() {
            return initial;
        }
        let threads = pool.threads();
        let mut workers: Vec<Marker> = (0..threads).map(|_| Marker::new(self.epoch.get())).collect();
        Work::run(pool, initial, &mut workers, budget.map(|b| b.div_ceil(threads)))
    }

    fn start_major<'gc, R: Collect<'gc> + ?Sized>(&mut self, root: &R) {
        self.epoch.set(!self.epoch.get());
        self.phase.set(Phase::Mark);
        self.marked_at.set(self.old_bytes());
        let mut m = Marker::new(self.epoch.get());
        Trace::new(&mut m).trace(root);
        self.gray.get_mut().append(&mut m.stack);
    }

    /// One marking step; returns `true` once the gray objects are exhausted
    fn step(&mut self, pool: &dyn Pool) -> bool {
        let old = self.old_bytes();
        let budget = MARK_STEP.max(old.saturating_sub(self.marked_at.get()) * MARK_RATIO);
        self.marked_at.set(old);
        let initial = take(self.gray.get_mut());
        let left = self.mark(pool, initial, Some(budget));
        *self.gray.get_mut() = left;
        self.gray.get_mut().is_empty()
    }

    // ---- Minor collection ----

    /// Minor collection: starting from the root and the remembered set, moves live young objects into the old
    /// generation in parallel, drops the rest, and empties the young generation
    pub(crate) fn minor<'gc, R: Collect<'gc> + ?Sized>(&mut self, pool: &dyn Pool, root: &mut R) {
        let (epoch, marking) = (self.epoch.get(), self.phase.get() == Phase::Mark);
        let remembered = take(self.remembered.get_mut());
        let (gray, promoted) = {
            let mut workers: Vec<Promoter<'_>> = (0..pool.threads()).map(|_| Promoter::new(&self.old, epoch, marking)).collect();
            // There is only one root, updated on the calling thread; the young objects it references directly
            // become the initial work after being moved
            Relocate::new(&mut workers[0]).relocate(root);
            let mut initial = take(&mut workers[0].stack);
            for p in remembered {
                p.header().and_flags(!Flags::REMEMBERED);
                if p.vtable().needs_trace {
                    initial.push(p);
                }
            }
            let left = Work::run(pool, initial, &mut workers, None);
            debug_assert!(left.is_empty());
            let gray: Vec<Ptr> = workers.iter_mut().flat_map(|w| take(&mut w.gray)).collect();
            (gray, workers.iter().map(|w| w.promoted).sum::<usize>())
        };
        // Whatever was not moved is dead: those that can be dropped on the thread pool are dropped in parallel,
        // the rest are left to the calling thread
        let mut drops = take(&mut self.nursery.get_mut().drops);
        drops.retain(|p| p.header().forwarded().is_none());
        let parallel = self.parallel_drop;
        let serial: Vec<Vec<Ptr>> = split(if parallel { pool } else { &moremath::ThreadSingle }, &mut drops, &|chunk: &mut [Ptr]| {
            let (anywhere, here): (Vec<Ptr>, Vec<Ptr>) = chunk.iter().partition(|p| parallel && p.vtable().parallel_drop);
            for p in anywhere {
                // SAFETY: unreachable and dropped only this once; the type allows dropping on this thread
                unsafe { p.drop_value() };
            }
            here
        });
        for p in serial.into_iter().flatten() {
            // SAFETY: as above, on the calling thread
            unsafe { p.drop_value() };
        }
        let g = self.gray.get_mut();
        if marking {
            // Young objects in the gray queue: live ones are already queued again in the old generation; dead ones are
            // discarded
            g.retain(|p| p.header().flags() & Flags::OLD != 0);
            g.extend(gray);
        }
        self.nursery.get_mut().reset();
        self.cursor.set(0);
        self.end.set(0);
        self.metrics.minor.set(self.metrics.minor.get() + 1);
        self.metrics.promoted.set(self.metrics.promoted.get() + promoted);
        self.verify("after minor");
    }

    // ---- Finishing the major collection ----

    /// Finishes marking (parallel, atomic), sweeps (parallel), and compacts when worthwhile (parallel)
    fn finish<'gc, R: Collect<'gc> + ?Sized>(&mut self, pool: &dyn Pool, root: &mut R) {
        self.minor(pool, root);
        let mut m = Marker::new(self.epoch.get());
        Trace::new(&mut m).trace(&*root);
        let mut initial = take(self.gray.get_mut());
        initial.append(&mut m.stack);
        let left = self.mark(pool, initial, None);
        debug_assert!(left.is_empty());
        let live = self.old.get_mut().sweep(pool, self.epoch.get(), self.parallel_drop);
        self.verify("after sweep");
        self.phase.set(Phase::Sleep);
        self.compact(pool, root);
        self.verify("after compact");
        self.threshold.set(MIN_HEAP.max(live * GROWTH));
        self.metrics.major.set(self.metrics.major.get() + 1);
    }

    /// Evacuates sparse pages (parallel copy), then updates every field pointing into them (parallel)
    fn compact<'gc, R: Collect<'gc> + ?Sized>(&mut self, pool: &dyn Pool, root: &mut R) {
        let mut moving = self.old.get_mut().select_evacuation();
        if moving.is_empty() {
            return;
        }
        let flags = Flags::OLD | marked_bit(self.epoch.get());
        let moved = moving.iter().map(|&(_, class)| SizeClass::SLOTS[class]).sum();
        let old = &self.old;
        split(pool, &mut moving, &|chunk: &mut [(Ptr, usize)]| {
            let mut lab = Lab::new(old);
            for &(p, class) in &*chunk {
                let vt = p.vtable();
                let q = lab.slot(class);
                // SAFETY: a free slot owned by this task; p is in a page being evacuated and appears only once
                unsafe { p.copy_to(vt, q, flags) };
                p.header().forward(q);
            }
        });
        Relocate::new(&mut Compactor).relocate(root);
        let mut live = self.old.get_mut().live();
        split(pool, &mut live, &|chunk: &mut [Ptr]| {
            for &p in chunk.iter().filter(|p| p.vtable().needs_trace) {
                // SAFETY: live old objects, each owned by one task
                unsafe { p.relocate(&mut Relocate::new(&mut Compactor)) };
            }
        });
        self.old.get_mut().release_evacuated(moved);
        self.metrics.compactions.set(self.metrics.compactions.get() + 1);
    }

    // ---- Pacing ----

    /// Does a step according to the allocation debt: a minor collection when the young generation is full; starts /
    /// advances the major collection once the old generation passes its threshold
    pub(crate) fn collect_debt<'gc, R: Collect<'gc> + ?Sized>(&mut self, pool: &dyn Pool, root: &mut R) {
        if self.young_bytes() >= NURSERY_LIMIT {
            self.minor(pool, root);
        }
        match self.phase.get() {
            Phase::Sleep if self.old_bytes() >= self.threshold.get() => self.start_major(root),
            Phase::Mark if self.step(pool) => self.finish(pool, root),
            Phase::Sleep | Phase::Mark => {}
        }
    }

    /// One full collection cycle (including minor collection and compaction)
    pub(crate) fn collect_all<'gc, R: Collect<'gc> + ?Sized>(&mut self, pool: &dyn Pool, root: &mut R) {
        if self.phase.get() == Phase::Sleep {
            self.start_major(root);
        }
        self.finish(pool, root);
    }

    // ---- Debug verification (feature `verify`) ----

    /// Every edge of every old-generation object: the target must be a live, unmoved, non-free old object (the young
    /// generation must be empty)
    #[allow(clippy::unused_self)]
    fn verify(&mut self, stage: &'static str) {
        #[cfg(feature = "verify")]
        {
            /// Verifies one edge
            struct Verifier<'a> {
                old: &'a OldSpace,
                stage: &'static str,
                owner: Ptr,
            }
            impl Visit for Verifier<'_> {
                fn visit(&mut self, p: Ptr) {
                    let place = self.old.place(p);
                    assert!(place != "unknown", "{}: edge to unknown memory {p:?} from {:?}", self.stage, self.owner);
                    let f = p.header().flags();
                    assert!(f & (Flags::FORWARDED | Flags::FREE) == 0 && f & Flags::OLD != 0, "{}: edge to {p:?} ({place}) flags {f:#x}", self.stage);
                }
            }
            let old = self.old.get_mut();
            for p in old.live().into_iter().filter(|p| p.vtable().needs_trace) {
                let mut v = Verifier { old, stage, owner: p };
                // SAFETY: a live old object
                unsafe { p.trace(&mut Trace::new(&mut v)) };
            }
        }
        let _ = stage;
    }
}

impl Drop for Context {
    fn drop(&mut self) {
        for p in take(&mut self.nursery.get_mut().drops) {
            if p.header().forwarded().is_none() {
                // SAFETY: the whole heap is being torn down; each young object is dropped once (old ones are
                // dropped by OldSpace)
                unsafe { p.drop_value() };
            }
        }
    }
}
