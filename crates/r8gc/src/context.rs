//! The collector: shared state the mutators read (atomic, lock-free) and the collection phases (marking, minor
//! collection, sweeping, compaction; parallel on the thread pool, while no mutator runs). The mutator side
//! (allocation, write barrier) is [`crate::Mutation`].
use alloc::sync::Arc;
use alloc::vec::Vec;
use core::mem::take;
use core::sync::atomic::{AtomicBool, Ordering};

use r8lockfree::PushList;

use crate::collect::{Mover, Visit};
use crate::header::{Claim, Flags, Ptr, SizeClass, VTable};
use crate::heap::{Lab, Memory, NURSERY_LIMIT, Nursery, OldSpace};
use crate::parallel::{Pool, Work, Worker, split};
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
pub(crate) fn marked_bit(epoch: bool) -> u32 {
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
    fn new(old: &'a OldSpace, epoch: bool, marking: bool) -> Self {
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
    /// A major collection is marking. Changes only during collection, while no mutator runs; mutators copy it when
    /// they enter `mutate`
    marking: AtomicBool,
    /// The mark bit counts as marked only when equal to this (changes only during collection, like `marking`)
    epoch: AtomicBool,
    nursery: Nursery,
    old: OldSpace,
    /// Handed in by mutators as they leave `mutate` (lock-free): objects the barrier grayed again, old objects that
    /// may point into the young generation, young objects with drop glue
    gray_in: PushList<Vec<Ptr>>,
    remembered_in: PushList<Vec<Ptr>>,
    drops_in: PushList<Vec<Ptr>>,
    /// The collector's gray queue (touched only during collection)
    gray: Vec<Ptr>,
    /// A major collection starts when the old generation grows this large
    threshold: usize,
    /// Size of the old generation at the previous marking step
    marked_at: usize,
    /// Dead objects of eligible types are dropped on the thread pool (worthwhile only when the allocator frees fast
    /// across threads)
    pub(crate) parallel_drop: bool,
    /// Shared with the heap handle, which reads it without touching the context
    pub(crate) metrics: Arc<Metrics>,
}

impl Context {
    pub(crate) fn new() -> Self {
        Self {
            marking: AtomicBool::new(false),
            epoch: AtomicBool::new(false),
            nursery: Nursery::default(),
            old: OldSpace::default(),
            gray_in: PushList::default(),
            remembered_in: PushList::default(),
            drops_in: PushList::default(),
            gray: Vec::new(),
            threshold: MIN_HEAP,
            marked_at: 0,
            parallel_drop: false,
            metrics: Arc::default(),
        }
    }

    pub(crate) fn phase(&self) -> Phase {
        if self.marking() { Phase::Mark } else { Phase::Sleep }
    }

    pub(crate) fn marking(&self) -> bool {
        self.marking.load(Ordering::Acquire)
    }

    pub(crate) fn epoch(&self) -> bool {
        self.epoch.load(Ordering::Acquire)
    }

    pub(crate) fn young_bytes(&self) -> usize {
        self.nursery.used()
    }

    pub(crate) fn old_bytes(&self) -> usize {
        self.old.bytes()
    }

    /// Which young generation is current: allocation buffers from an earlier one point into reset blocks
    pub(crate) fn generation(&self) -> usize {
        self.metrics.minor_count()
    }

    /// A fresh young-generation block for a mutator's allocation buffer, as `[start, end)`
    pub(crate) fn refill(&self) -> (usize, usize) {
        self.nursery.refill()
    }

    /// A large object (header not written): straight into the old generation
    pub(crate) fn alloc_large(&self, vt: &VTable) -> Ptr {
        self.old.alloc_large(vt)
    }

    /// A mutator leaving `mutate` hands in what it recorded
    pub(crate) fn hand_in(&self, gray: Vec<Ptr>, remembered: Vec<Ptr>, drops: Vec<Ptr>) {
        for (list, v) in [(&self.gray_in, gray), (&self.remembered_in, remembered), (&self.drops_in, drops)] {
            if !v.is_empty() {
                // These lists are never sealed
                let _ = list.push(v);
            }
        }
    }

    /// Objects grayed again by mutators join the collector's gray queue
    fn absorb_gray(&mut self) {
        let more = self.gray_in.take_concat();
        if self.gray.is_empty() {
            self.gray = more;
        } else {
            self.gray.extend(more);
        }
    }

    // ---- Marking ----

    /// Parallel marking starting from `initial`; with a `budget`, stops after that many bytes and returns the
    /// unfinished work
    fn mark(&self, pool: &dyn Pool, initial: Vec<Ptr>, budget: Option<usize>) -> Vec<Ptr> {
        if initial.is_empty() {
            return initial;
        }
        let threads = pool.threads();
        let mut workers: Vec<Marker> = (0..threads).map(|_| Marker::new(self.epoch())).collect();
        Work::run(pool, initial, &mut workers, budget.map(|b| b.div_ceil(threads)))
    }

    fn start_major<'gc, R: Collect<'gc> + ?Sized>(&mut self, root: &R) {
        self.epoch.store(!self.epoch(), Ordering::Release);
        self.marking.store(true, Ordering::Release);
        self.marked_at = self.old_bytes();
        let mut m = Marker::new(self.epoch());
        Trace::new(&mut m).trace(root);
        self.gray.append(&mut m.stack);
    }

    /// One marking step; returns `true` once the gray objects are exhausted
    fn step(&mut self, pool: &dyn Pool) -> bool {
        self.absorb_gray();
        let old = self.old_bytes();
        let budget = MARK_STEP.max(old.saturating_sub(self.marked_at) * MARK_RATIO);
        self.marked_at = old;
        let initial = take(&mut self.gray);
        self.gray = self.mark(pool, initial, Some(budget));
        self.gray.is_empty()
    }

    // ---- Minor collection ----

    /// Minor collection: starting from the root and the remembered set, moves live young objects into the old
    /// generation in parallel, drops the rest, and empties the young generation
    pub(crate) fn minor<'gc, R: Collect<'gc> + ?Sized>(&mut self, pool: &dyn Pool, root: &mut R) {
        let (epoch, marking) = (self.epoch(), self.marking());
        // Slots handed back by the last parallel phase become claimable again
        self.old.settle();
        let remembered = self.remembered_in.take_concat();
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
        let mut drops = self.drops_in.take_concat();
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
        self.absorb_gray();
        let g = &mut self.gray;
        if marking {
            // Young objects in the gray queue: live ones are already queued again in the old generation; dead ones are
            // discarded
            g.retain(|p| p.header().flags() & Flags::OLD != 0);
            g.extend(gray);
        }
        self.nursery.reset();
        // Advancing the generation invalidates every allocation buffer handed out from the reset blocks
        self.metrics.minor.fetch_add(1, Ordering::AcqRel);
        self.metrics.promoted.fetch_add(promoted, Ordering::Relaxed);
        self.verify("after minor");
    }

    // ---- Finishing the major collection ----

    /// Finishes marking (parallel, atomic), sweeps (parallel), and compacts when worthwhile (parallel)
    fn finish<'gc, R: Collect<'gc> + ?Sized>(&mut self, pool: &dyn Pool, root: &mut R) {
        self.minor(pool, root);
        let mut m = Marker::new(self.epoch());
        Trace::new(&mut m).trace(&*root);
        self.absorb_gray();
        let mut initial = take(&mut self.gray);
        initial.append(&mut m.stack);
        let left = self.mark(pool, initial, None);
        debug_assert!(left.is_empty());
        let live = self.old.sweep(pool, self.epoch(), self.parallel_drop);
        self.verify("after sweep");
        self.marking.store(false, Ordering::Release);
        self.compact(pool, root);
        self.verify("after compact");
        self.threshold = MIN_HEAP.max(live * GROWTH);
        // Pages emptied by this cycle have been handed back: give their memory to the OS
        Memory::trim();
        self.metrics.major.fetch_add(1, Ordering::Relaxed);
    }

    /// Evacuates sparse pages (parallel copy), then updates every field pointing into them (parallel)
    fn compact<'gc, R: Collect<'gc> + ?Sized>(&mut self, pool: &dyn Pool, root: &mut R) {
        let mut moving = self.old.select_evacuation();
        if moving.is_empty() {
            return;
        }
        let flags = Flags::OLD | marked_bit(self.epoch());
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
        let mut live = self.old.live();
        split(pool, &mut live, &|chunk: &mut [Ptr]| {
            for &p in chunk.iter().filter(|p| p.vtable().needs_trace) {
                // SAFETY: live old objects, each owned by one task
                unsafe { p.relocate(&mut Relocate::new(&mut Compactor)) };
            }
        });
        self.old.release_evacuated(moved);
        self.metrics.compactions.fetch_add(1, Ordering::Relaxed);
    }

    // ---- Pacing ----

    /// Does a step according to the allocation debt: a minor collection when the young generation is full; starts /
    /// advances the major collection once the old generation passes its threshold
    pub(crate) fn collect_debt<'gc, R: Collect<'gc> + ?Sized>(&mut self, pool: &dyn Pool, root: &mut R) {
        if self.young_bytes() >= NURSERY_LIMIT {
            self.minor(pool, root);
        }
        match self.phase() {
            Phase::Sleep if self.old_bytes() >= self.threshold => self.start_major(root),
            Phase::Mark if self.step(pool) => self.finish(pool, root),
            Phase::Sleep | Phase::Mark => {}
        }
    }

    /// One full collection cycle (including minor collection and compaction)
    pub(crate) fn collect_all<'gc, R: Collect<'gc> + ?Sized>(&mut self, pool: &dyn Pool, root: &mut R) {
        if !self.marking() {
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
            let old = &self.old;
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
        for p in self.drops_in.take().flatten() {
            if p.header().forwarded().is_none() {
                // SAFETY: the whole heap is being torn down; each young object is dropped once (old ones are
                // dropped by OldSpace)
                unsafe { p.drop_value() };
            }
        }
    }
}
