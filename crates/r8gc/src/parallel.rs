//! Parallelism: the thread pool interface (any moremath `Executor` qualifies), backoff, and the lock-free
//! work-stealing driver.
use alloc::boxed::Box;
use alloc::vec::Vec;
use core::hint::spin_loop;
use core::sync::atomic::{AtomicUsize, Ordering};

use moremath::Executor;
use r8lockfree::{Deque, Owner};

use crate::header::Ptr;

/// A task handed to the thread pool: borrows data for `'t`, and the call returns only after it has run.
pub type Job<'t> = Box<dyn FnOnce() + Send + 't>;

/// The thread pool used for collection. moremath's `ThreadSingle` / `ThreadMulti` both implement it;
/// embedders can also implement it with their own pool (several heaps can share one pool).
pub trait Pool {
    /// How many tasks can run concurrently
    fn threads(&self) -> usize;

    /// Returns only after `jobs` have finished (tasks may borrow the caller's data)
    fn run(&self, jobs: Vec<Job<'_>>);
}

impl<X: Executor> Pool for X {
    fn threads(&self) -> usize {
        Executor::threads(self)
    }

    fn run(&self, jobs: Vec<Job<'_>>) {
        self.execute(jobs);
    }
}

/// Waiting for other threads: spin first, then yield the CPU after a while (so we don't spin uselessly when there
/// are more threads than cores and the lock holder has been descheduled).
#[derive(Default)]
pub(crate) struct Backoff(u32);

impl Backoff {
    /// After this many spins, switch to yielding the CPU
    const SPINS: u32 = 64;

    pub(crate) fn wait(&mut self) {
        if self.0 < Self::SPINS {
            self.0 += 1;
            spin_loop();
        } else {
            #[cfg(feature = "std")]
            std::thread::yield_now();
            #[cfg(not(feature = "std"))]
            spin_loop();
        }
    }
}

/// Splits `items` into chunks processed in parallel; results are returned in chunk order.
pub(crate) fn split<T: Send, R: Send>(pool: &dyn Pool, items: &mut [T], f: &(dyn Fn(&mut [T]) -> R + Sync)) -> Vec<R> {
    /// Chunks per thread (smaller chunks let threads that finish early take more, balancing the load)
    const CHUNKS_PER_THREAD: usize = 4;
    /// Below this many, do the work on the calling thread without involving the thread pool
    const SOLO: usize = 256;
    if items.is_empty() {
        return Vec::new();
    }
    if items.len() < SOLO || pool.threads() == 1 {
        return alloc::vec![f(items)];
    }
    let size = items.len().div_ceil(pool.threads() * CHUNKS_PER_THREAD).max(1);
    let mut out: Vec<Option<R>> = items.chunks(size).map(|_| None).collect();
    let jobs: Vec<Job<'_>> = items.chunks_mut(size).zip(out.iter_mut()).map(|(chunk, slot)| Box::new(move || *slot = Some(f(chunk))) as Job<'_>).collect();
    pool.run(jobs);
    out.into_iter().map(|r| r.expect("job ran")).collect()
}

/// A worker in work stealing: has its own stack and processes one object at a time.
pub(crate) trait Worker: Send {
    fn stack(&mut self) -> &mut Vec<Ptr>;

    /// Processes one object (possibly pushing new ones onto its own stack); returns how many bytes of work were done
    fn process(&mut self, p: Ptr) -> usize;
}

/// Work stealing: each worker has a private stack; when it grows large, half is moved into the worker's own
/// Chase–Lev deque, which idle workers steal from. Finishes when every started worker is idle. Lock-free.
pub(crate) struct Work {
    deques: Vec<Deque<Ptr>>,
    /// Objects in the deques, counted before they go in and after they come out (never undercounts)
    pending: AtomicUsize,
    /// Workers that started (when the pool is busy some start late, even after the others have finished)
    started: AtomicUsize,
    idle: AtomicUsize,
}

impl Work {
    /// Only share once the private stack exceeds this; also how many a refill takes at most
    const SHARE: usize = 64;
    /// The calling thread works alone on this many objects first: little work doesn't wake the pool
    const SOLO: usize = 4096;
    /// An idle worker retires after this many rounds without work (instead of spinning to the end)
    const RETIRE: usize = 1 << 10;

    /// Lets `workers` finish `initial` (and whatever turns up along the way) on `pool`.
    /// With a `budget`, each worker stops after that many bytes; what's left is returned.
    pub(crate) fn run<W: Worker>(pool: &dyn Pool, mut initial: Vec<Ptr>, workers: &mut [W], mut budget: Option<usize>) -> Vec<Ptr> {
        let first = &mut workers[0];
        first.stack().append(&mut initial);
        for _ in 0..Self::SOLO {
            let Some(p) = first.stack().pop() else { return Vec::new() };
            let done = first.process(p).max(1);
            if let Some(b) = &mut budget {
                *b = b.saturating_sub(done);
                if *b == 0 {
                    return core::mem::take(first.stack());
                }
            }
        }
        let initial = core::mem::take(first.stack());
        if initial.is_empty() {
            return initial;
        }
        let work = Self { deques: workers.iter().map(|_| Deque::default()).collect(), pending: AtomicUsize::new(initial.len()), started: AtomicUsize::new(0), idle: AtomicUsize::new(0) };
        // Everything left goes into the first deque: not pre-assigned to workers that may start late
        let owner = work.deques[0].owner().expect("fresh deque");
        for p in initial {
            owner.push(p);
        }
        drop(owner);
        let work = &work;
        let jobs: Vec<Job<'_>> = workers.iter_mut().enumerate().map(|(me, w)| Box::new(move || work.drive(me, w, budget)) as Job<'_>).collect();
        pool.run(jobs);
        let mut left: Vec<Ptr> = work.deques.iter().flat_map(|d| core::iter::from_fn(|| d.steal())).collect();
        for w in workers.iter_mut() {
            left.append(w.stack());
        }
        left
    }

    fn drive<W: Worker>(&self, me: usize, w: &mut W, mut budget: Option<usize>) {
        let owner = self.deques[me].owner().expect("one owner per deque");
        self.started.fetch_add(1, Ordering::SeqCst);
        loop {
            while let Some(p) = w.stack().pop() {
                let done = w.process(p).max(1);
                if let Some(b) = &mut budget {
                    *b = b.saturating_sub(done);
                    if *b == 0 {
                        // Done enough: counts as permanently idle; the rest stays in the stack and deque
                        self.idle.fetch_add(1, Ordering::SeqCst);
                        return;
                    }
                }
                self.share(&owner, w.stack());
            }
            if self.refill(me, &owner, w.stack()) {
                continue;
            }
            self.idle.fetch_add(1, Ordering::SeqCst);
            let mut backoff = Backoff::default();
            for spins in 0.. {
                if spins == Self::RETIRE {
                    // Retire: stays counted as idle; its deque is empty (it found nothing), others carry on
                    return;
                }
                if self.pending.load(Ordering::SeqCst) > 0 {
                    self.idle.fetch_sub(1, Ordering::SeqCst);
                    break;
                }
                // Every started worker is idle and the deques are empty: done (latecomers leave empty-handed)
                if self.idle.load(Ordering::SeqCst) == self.started.load(Ordering::SeqCst) {
                    return;
                }
                backoff.wait();
            }
        }
    }

    /// The private stack grew large while little is shared: move half into our deque
    fn share(&self, owner: &Owner<'_, Ptr>, stack: &mut Vec<Ptr>) {
        if stack.len() > 2 * Self::SHARE && self.pending.load(Ordering::Relaxed) < self.deques.len() * Self::SHARE {
            let n = stack.len() / 2;
            self.pending.fetch_add(n, Ordering::SeqCst);
            stack.drain(..n).for_each(|p| owner.push(p));
        }
    }

    /// Takes up to `SHARE` objects: from our own deque first, then stealing from the others
    fn refill(&self, me: usize, owner: &Owner<'_, Ptr>, stack: &mut Vec<Ptr>) -> bool {
        if self.pending.load(Ordering::SeqCst) == 0 {
            return false;
        }
        let before = stack.len();
        stack.extend(core::iter::from_fn(|| owner.pop()).take(Self::SHARE));
        let n = self.deques.len();
        for k in 1..n {
            if stack.len() > before {
                break;
            }
            let d = &self.deques[(me + k) % n];
            stack.extend(core::iter::from_fn(|| d.steal()).take(Self::SHARE));
        }
        let got = stack.len() - before;
        self.pending.fetch_sub(got, Ordering::SeqCst);
        got > 0
    }
}
