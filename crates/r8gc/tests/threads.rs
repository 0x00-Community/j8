//! Several threads mutating one heap at once while others collect through the safepoint gate: every list keeps its
//! values; dead objects are dropped exactly once. Run with `--features verify` to check the heap after each phase.
use std::num::NonZeroUsize;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::thread;

use moremath::ThreadMulti;
use r8gc::{Collect, DynamicRoot, DynamicRootSet, Gc, Lock, R8Heap, Rootable, Tlab};

/// Mutating threads
const THREADS: usize = if cfg!(miri) { 2 } else { 4 };
/// Lists each thread builds
#[cfg(not(miri))]
const ROUNDS: usize = 40;
#[cfg(miri)]
const ROUNDS: usize = 2;
/// Nodes per list
#[cfg(not(miri))]
const LEN: usize = 2_000;
#[cfg(miri)]
const LEN: usize = 8;
/// Lists each thread keeps alive at a time
const KEEP: usize = 3;
/// Each thread collects after this many allocations
const COLLECT_EVERY: usize = if cfg!(miri) { 8 } else { 512 };
/// Collection threads in the pool
const POOL: NonZeroUsize = NonZeroUsize::new(if cfg!(miri) { 1 } else { 2 }).unwrap();

#[derive(Collect)]
#[collect(no_drop)]
struct Node<'gc> {
    value: usize,
    next: Option<Gc<'gc, Node<'gc>>>,
}

#[derive(Collect)]
#[collect(no_drop)]
struct Root<'gc> {
    roots: DynamicRootSet<'gc>,
}

type Heap<'p> = R8Heap<'p, Rootable![Root<'_>]>;
type ListRoot = DynamicRoot<Rootable![Node<'_>]>;
type CountedRoot = DynamicRoot<Rootable![Counted]>;

fn heap(pool: &ThreadMulti) -> Heap<'_> {
    R8Heap::new(pool, |mc| Root { roots: DynamicRootSet::new(mc) })
}

/// (length, sum of values) of a list
fn walk<'gc>(head: Gc<'gc, Node<'gc>>) -> (usize, usize) {
    let (mut count, mut total, mut at) = (0, 0, Some(head));
    while let Some(n) = at {
        (count, total, at) = (count + 1, total + n.value, n.next);
    }
    (count, total)
}

/// What a list starting at `base` must hold
fn expected(base: usize) -> (usize, usize) {
    (LEN, LEN * base + LEN * (LEN - 1) / 2)
}

#[test]
fn mutators_allocate_while_others_collect() {
    let pool = ThreadMulti::from(POOL);
    let heap = heap(&pool);
    let heap = &heap;
    let done = AtomicBool::new(false);
    thread::scope(|s| {
        // A thread that keeps collecting fully while the mutators run
        s.spawn(|| {
            // Bounded under Miri (it is slow); otherwise until the mutators finish
            for _ in 0..if cfg!(miri) { 2 } else { usize::MAX } {
                if done.load(Ordering::Acquire) {
                    break;
                }
                heap.collect_all_shared();
                thread::yield_now();
            }
        });
        let workers: Vec<_> = (0..THREADS)
            .map(|t| {
                s.spawn(move || {
                    let mut tlab = Tlab::default();
                    let mut kept: Vec<(ListRoot, usize)> = Vec::new();
                    for r in 0..ROUNDS {
                        let base = (t * ROUNDS + r) * LEN;
                        let list = heap.mutate_in(&mut tlab, |mc, root| {
                            let mut head = Gc::new(mc, Node { value: base, next: None });
                            for i in 1..LEN {
                                head = Gc::new(mc, Node { value: base + i, next: Some(head) });
                                // Garbage between the live nodes
                                Gc::new(mc, Node { value: 0, next: None });
                            }
                            root.roots.stash(mc, head)
                        });
                        kept.push((list, base));
                        if kept.len() > KEEP {
                            kept.remove(0);
                        }
                        // Every kept list survived the collections so far intact
                        heap.mutate_in(&mut tlab, |_, root| {
                            for (list, base) in &kept {
                                assert_eq!(walk(root.roots.fetch(list)), expected(*base), "thread {t} round {r}");
                            }
                        });
                        heap.collect_debt_shared();
                    }
                })
            })
            .collect();
        for w in workers {
            w.join().unwrap();
        }
        done.store(true, Ordering::Release);
    });
    assert!(heap.metrics().minor_count() > 0 && heap.metrics().major_count() > 0);
}

/// Counts on drop
#[derive(Collect)]
#[collect(require_static)]
struct Counted(Arc<AtomicUsize>);

impl Drop for Counted {
    fn drop(&mut self) {
        self.0.fetch_add(1, Ordering::Relaxed);
    }
}

#[test]
fn drops_exactly_once_across_threads() {
    /// Objects each thread allocates
    const EACH: usize = if cfg!(miri) { 16 } else { 4_000 };
    let pool = ThreadMulti::from(POOL);
    let heap = heap(&pool);
    let dropped = Arc::new(AtomicUsize::new(0));
    let kept: Vec<CountedRoot> = thread::scope(|s| {
        let workers: Vec<_> = (0..THREADS)
            .map(|_| {
                let (heap, dropped) = (&heap, &dropped);
                s.spawn(move || {
                    let mut tlab = Tlab::default();
                    let mut kept = Vec::new();
                    for i in 0..EACH {
                        let c = heap.mutate_in(&mut tlab, |mc, root| {
                            let c = Gc::new(mc, Counted(dropped.clone()));
                            (i % 2 == 0).then(|| root.roots.stash(mc, c))
                        });
                        kept.extend(c);
                        if i % COLLECT_EVERY == 0 {
                            heap.collect_debt_shared();
                        }
                    }
                    kept
                })
            })
            .collect();
        workers.into_iter().flat_map(|w| w.join().unwrap()).collect()
    });
    heap.collect_all_shared();
    assert_eq!(dropped.load(Ordering::Relaxed), THREADS * EACH / 2);
    heap.collect_all_shared();
    assert_eq!(dropped.load(Ordering::Relaxed), THREADS * EACH / 2);
    drop(heap);
    assert_eq!(dropped.load(Ordering::Relaxed), THREADS * EACH);
    drop(kept);
}

#[derive(Collect)]
#[collect(no_drop)]
struct Shared<'gc> {
    /// Top of a lock-free stack living in the heap
    top: Gc<'gc, Lock<Option<Gc<'gc, Node<'gc>>>>>,
}

/// Threads push onto one in-heap stack with compare-exchange while others collect: nothing is lost, and the old
/// cell receiving young pointers keeps them alive (write barrier).
#[test]
fn atomic_cells_shared_across_threads() {
    /// Nodes each thread pushes
    const EACH: usize = if cfg!(miri) { 8 } else { 5_000 };
    let pool = ThreadMulti::from(POOL);
    let heap: R8Heap<'_, Rootable![Shared<'_>]> = R8Heap::new(&pool, |mc| Shared { top: Gc::new(mc, Lock::new(None)) });
    let heap = &heap;
    // The cell is old before anyone pushes young nodes into it
    heap.collect_all_shared();
    thread::scope(|s| {
        for t in 0..THREADS {
            s.spawn(move || {
                let mut tlab = Tlab::default();
                for i in 0..EACH {
                    heap.mutate_in(&mut tlab, |mc, root| {
                        let mut seen = root.top.get();
                        loop {
                            let node = Gc::new(mc, Node { value: t * EACH + i, next: seen });
                            match root.top.compare_exchange(mc, seen, Some(node)) {
                                Ok(_) => break,
                                Err(now) => seen = now,
                            }
                        }
                    });
                    if i % COLLECT_EVERY == 0 {
                        heap.collect_debt_shared();
                    }
                }
            });
        }
        s.spawn(|| {
            for _ in 0..if cfg!(miri) { 1 } else { 20 } {
                heap.collect_all_shared();
                thread::yield_now();
            }
        });
    });
    heap.collect_all_shared();
    let n = THREADS * EACH;
    let (count, total) = heap.mutate(|_, root| root.top.get().map_or((0, 0), walk));
    assert_eq!((count, total), (n, n * (n - 1) / 2));
}
