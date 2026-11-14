//! Parallelism: the thread pool interface (any moremath `Executor` qualifies), a spin lock, and the work-stealing
//! driver.
use alloc::boxed::Box;
use alloc::vec::Vec;
use core::cell::UnsafeCell;
use core::hint::spin_loop;
use core::ops::{Deref, DerefMut};
use core::sync::atomic::{AtomicBool, AtomicUsize, Ordering};

use moremath::Executor;

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

/// A spin lock for very short critical sections (`no_std`).
pub(crate) struct Spin<T> {
    locked: AtomicBool,
    value: UnsafeCell<T>,
}

// SAFETY: only the thread holding the lock touches value at any time
unsafe impl<T: Send> Sync for Spin<T> {}
unsafe impl<T: Send> Send for Spin<T> {}

impl<T: Default> Default for Spin<T> {
    fn default() -> Self {
        Self::new(T::default())
    }
}

impl<T> Spin<T> {
    pub(crate) fn new(value: T) -> Self {
        Self { locked: AtomicBool::new(false), value: UnsafeCell::new(value) }
    }

    pub(crate) fn lock(&self) -> SpinGuard<'_, T> {
        let mut backoff = Backoff::default();
        while self.locked.compare_exchange_weak(false, true, Ordering::Acquire, Ordering::Relaxed).is_err() {
            backoff.wait();
        }
        SpinGuard(self)
    }

    pub(crate) fn get_mut(&mut self) -> &mut T {
        self.value.get_mut()
    }
}

pub(crate) struct SpinGuard<'a, T>(&'a Spin<T>);

impl<T> Deref for SpinGuard<'_, T> {
    type Target = T;

    fn deref(&self) -> &T {
        // SAFETY: the lock is held
        unsafe { &*self.0.value.get() }
    }
}

impl<T> DerefMut for SpinGuard<'_, T> {
    fn deref_mut(&mut self) -> &mut T {
        // SAFETY: the lock is held
        unsafe { &mut *self.0.value.get() }
    }
}

impl<T> Drop for SpinGuard<'_, T> {
    fn drop(&mut self) {
        self.0.locked.store(false, Ordering::Release);
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

/// Work stealing: each worker has a local stack; when it grows large, half is moved to the shared stack, and an
/// empty worker takes from the shared stack. Finishes when every worker is idle.
pub(crate) struct Work {
    shared: Spin<Vec<Ptr>>,
    /// Number of items in the shared stack (readable without taking the lock)
    pending: AtomicUsize,
    /// Number of workers that have started (when the pool is busy, some workers start late, possibly after everyone
    /// else has finished)
    started: AtomicUsize,
    idle: AtomicUsize,
    workers: usize,
}

impl Work {
    /// Only share out when the local stack exceeds this
    const SHARE: usize = 64;
    /// The calling thread first processes this many objects alone: with little work, the pool is not involved
    const SOLO: usize = 4096;
    /// An idle worker retires after waiting this many rounds without work (instead of spinning until the end)
    const RETIRE: usize = 1 << 10;

    /// Uses `pool` to have `workers` process `initial` (and anything discovered along the way) together.
    /// With a `budget`, each worker stops after that many bytes; unfinished work is returned.
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
        // Put the rest on the shared stack: don't pre-assign it to workers that may start late
        let pending = AtomicUsize::new(initial.len());
        let work = Self { shared: Spin::new(initial), pending, started: AtomicUsize::new(0), idle: AtomicUsize::new(0), workers: workers.len() };
        let jobs: Vec<Job<'_>> = workers.iter_mut().map(|w| Box::new(|| work.drive(w, budget)) as Job<'_>).collect();
        pool.run(jobs);
        let mut left = work.shared.into_inner();
        for w in workers.iter_mut() {
            left.append(w.stack());
        }
        left
    }

    fn drive<W: Worker>(&self, w: &mut W, mut budget: Option<usize>) {
        self.started.fetch_add(1, Ordering::SeqCst);
        loop {
            while let Some(p) = w.stack().pop() {
                let done = w.process(p).max(1);
                if let Some(b) = &mut budget {
                    *b = b.saturating_sub(done);
                    if *b == 0 {
                        // Budget used up: count as permanently idle, leave the rest on the stack
                        self.idle.fetch_add(1, Ordering::SeqCst);
                        return;
                    }
                }
                self.share(w.stack());
            }
            if self.steal(w.stack()) {
                continue;
            }
            self.idle.fetch_add(1, Ordering::SeqCst);
            let mut backoff = Backoff::default();
            for spins in 0.. {
                if spins == Self::RETIRE {
                    // Retire: stay counted as idle; work shared out by others is taken back by those still working
                    return;
                }
                if self.pending.load(Ordering::SeqCst) > 0 {
                    self.idle.fetch_sub(1, Ordering::SeqCst);
                    break;
                }
                // All started workers are idle and the shared stack is empty: done (workers that haven't started
                // yet will leave empty-handed)
                if self.idle.load(Ordering::SeqCst) == self.started.load(Ordering::SeqCst) {
                    return;
                }
                backoff.wait();
            }
        }
    }

    /// The local stack is large and the shared stack is not: share half of it
    fn share(&self, stack: &mut Vec<Ptr>) {
        if stack.len() > 2 * Self::SHARE && self.pending.load(Ordering::Relaxed) < self.workers * Self::SHARE {
            let mut shared = self.shared.lock();
            shared.extend(stack.drain(..stack.len() / 2));
            self.pending.store(shared.len(), Ordering::SeqCst);
        }
    }

    /// Takes a batch from the shared stack
    fn steal(&self, stack: &mut Vec<Ptr>) -> bool {
        if self.pending.load(Ordering::SeqCst) == 0 {
            return false;
        }
        let mut shared = self.shared.lock();
        let take = shared.len().min(Self::SHARE);
        let from = shared.len() - take;
        stack.extend(shared.drain(from..));
        self.pending.store(shared.len(), Ordering::SeqCst);
        take > 0
    }
}

impl<T> Spin<T> {
    pub(crate) fn into_inner(self) -> T {
        self.value.into_inner()
    }
}
