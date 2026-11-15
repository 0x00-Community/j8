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
pub trait Pool: Sync {
    /// How many tasks can run concurrently
    fn threads(&self) -> usize;

    /// Returns only after `jobs` have finished (tasks may borrow the caller's data)
    fn run(&self, jobs: Vec<Job<'_>>);
}

impl<X: Executor + Sync> Pool for X {
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

/// A batch of objects moved between workers as one deque element: a `Box<Vec<Ptr>>` held as its address.
/// Whoever pushes it gives up the box; whoever pops or steals it takes the box back.
#[derive(Clone, Copy)]
struct Batch(usize);

impl From<usize> for Batch {
    fn from(addr: usize) -> Self {
        Self(addr)
    }
}

impl From<Batch> for usize {
    fn from(b: Batch) -> Self {
        b.0
    }
}

impl From<Box<Vec<Ptr>>> for Batch {
    fn from(v: Box<Vec<Ptr>>) -> Self {
        Self(Box::into_raw(v) as usize)
    }
}

impl From<Batch> for Box<Vec<Ptr>> {
    fn from(b: Batch) -> Self {
        // SAFETY: the address came from `Batch::from(Box)`, and each batch is popped or stolen exactly once
        unsafe { Box::from_raw(b.0 as *mut Vec<Ptr>) }
    }
}

/// A worker's emptied batch buffers, reused for its next shares (they migrate between workers with the batches;
/// each worker keeps whatever it emptied, so no shared pool is needed).
#[derive(Default)]
// The boxes are the batch handles that travel through the deques; unboxing them would cost an allocation per batch
#[allow(clippy::vec_box)]
struct Spares(Vec<Box<Vec<Ptr>>>);

impl Spares {
    /// Most buffers a worker keeps
    const KEEP: usize = 8;

    /// A batch holding `items` (reusing a spare buffer when there is one)
    fn fill(&mut self, items: impl Iterator<Item = Ptr>) -> Batch {
        let mut b = self.0.pop().unwrap_or_default();
        b.extend(items);
        Batch::from(b)
    }

    /// Empties `batch` onto `stack` and keeps its buffer
    fn drain(&mut self, batch: Batch, stack: &mut Vec<Ptr>) {
        let mut b = Box::<Vec<Ptr>>::from(batch);
        stack.append(&mut b);
        if self.0.len() < Self::KEEP {
            self.0.push(b);
        }
    }
}

/// Work stealing: each worker has a private stack; when it grows large, a batch of it is pushed into the worker's
/// own Chase–Lev deque, which idle workers steal batches from. Finishes when every started worker is idle.
/// Lock-free; one atomic hand-off moves a whole batch.
pub(crate) struct Work {
    deques: Vec<Deque<Batch>>,
    /// Batches in the deques, counted before they go in and after they come out (never undercounts)
    pending: AtomicUsize,
    /// Workers that started (when the pool is busy some start late, even after the others have finished)
    started: AtomicUsize,
    idle: AtomicUsize,
}

impl Work {
    /// Objects per batch; the private stack shares once it holds more than two batches
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
        let work = Self { deques: workers.iter().map(|_| Deque::default()).collect(), pending: AtomicUsize::new(0), started: AtomicUsize::new(0), idle: AtomicUsize::new(0) };
        // Everything left goes into the first deque in batches: not pre-assigned to workers that may start late
        let owner = work.deques[0].owner().expect("fresh deque");
        let mut spares = Spares::default();
        for chunk in initial.chunks(Self::SHARE) {
            work.pending.fetch_add(1, Ordering::SeqCst);
            owner.push(spares.fill(chunk.iter().copied()));
        }
        drop(owner);
        let work = &work;
        let jobs: Vec<Job<'_>> = workers.iter_mut().enumerate().map(|(me, w)| Box::new(move || work.drive(me, w, budget)) as Job<'_>).collect();
        pool.run(jobs);
        let mut left: Vec<Ptr> = work.deques.iter().flat_map(|d| core::iter::from_fn(|| d.steal())).flat_map(|b| *Box::<Vec<Ptr>>::from(b)).collect();
        for w in workers.iter_mut() {
            left.append(w.stack());
        }
        left
    }

    fn drive<W: Worker>(&self, me: usize, w: &mut W, mut budget: Option<usize>) {
        let owner = self.deques[me].owner().expect("one owner per deque");
        let mut spares = Spares::default();
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
                self.share(&owner, &mut spares, w.stack());
            }
            if self.refill(me, &owner, &mut spares, w.stack()) {
                continue;
            }
            self.idle.fetch_add(1, Ordering::SeqCst);
            let mut backoff = Backoff::default();
            for spins in 0.. {
                if spins == Self::RETIRE {
                    // Retire: stays counted as idle; its deque is empty (it found nothing), others carry on
                    return;
                }
                // Steal while still counted idle and leave idle only with work in hand: a failed steal (several
                // thieves on one victim) backs off instead of churning the shared counters
                if self.pending.load(Ordering::SeqCst) > 0 && self.refill(me, &owner, &mut spares, w.stack()) {
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

    /// The private stack holds more than two batches while little is shared: push the oldest batch to our deque
    fn share(&self, owner: &Owner<'_, Batch>, spares: &mut Spares, stack: &mut Vec<Ptr>) {
        if stack.len() > 2 * Self::SHARE && self.pending.load(Ordering::Relaxed) < self.deques.len() {
            self.pending.fetch_add(1, Ordering::SeqCst);
            owner.push(spares.fill(stack.drain(..Self::SHARE)));
        }
    }

    /// Takes one batch: from our own deque first, then stealing from the others
    fn refill(&self, me: usize, owner: &Owner<'_, Batch>, spares: &mut Spares, stack: &mut Vec<Ptr>) -> bool {
        if self.pending.load(Ordering::SeqCst) == 0 {
            return false;
        }
        let n = self.deques.len();
        let Some(batch) = owner.pop().or_else(|| (1..n).find_map(|k| self.deques[(me + k) % n].steal())) else { return false };
        self.pending.fetch_sub(1, Ordering::SeqCst);
        spares.drain(batch, stack);
        true
    }
}
