use alloc::boxed::Box;
use alloc::vec::Vec;
use core::time::Duration;

use moremath::Executor;

/// A task handed to the thread pool: borrows data for `'t`, and the call returns only after it has run.
pub type Job<'t> = Box<dyn FnOnce() + Send + 't>;

/// A background task handed to the platform to run (borrows live for `'t`).
pub type Task<'t> = Job<'t>;

/// The thread pool for parallel built-ins. moremath's `ThreadSingle` / `ThreadMulti` both implement it; embedders can
/// also implement it with their own pool.
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

/// Threads and clocks provided by the embedder. The thread pool is the supertrait [`Pool`] (parallel built-ins run on
/// it; the GC heap has its own collector threads).
pub trait Platform: Sync + Pool {
    /// Monotonic clock: time elapsed since the platform was created
    fn now(&self) -> Duration;

    /// Wall clock: time elapsed since the Unix epoch (`Date`); the epoch itself if unavailable.
    fn wall_clock(&self) -> Duration {
        Duration::ZERO
    }

    /// Module loading: `specifier` as written in `referrer` ⇒ (normalized path, source); `None` if not found.
    fn load_module(&self, referrer: &str, specifier: &str) -> Option<(alloc::string::String, alloc::string::String)> {
        let _ = (referrer, specifier);
        None
    }

    /// Output one line (`console.log`).
    fn print(&self, line: &str);
}
