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

/// What async host work settles to: run on the runtime's thread, it turns the result into a JS value (or an
/// exception) for the promise.
pub type Settle = Box<dyn for<'gc, 'x> FnOnce(&mut crate::Agent<'gc, 'x>) -> Result<crate::Value<'gc>, crate::Error> + Send>;

/// Async host work (an `async fn` bound with the binding macros): a future, and the promise's resolve / reject it
/// settles when done. It belongs to the isolate that made it (its handles are that isolate's): [`crate::Agent::spawn`]
/// queues it on the isolate, [`crate::Isolate::take_spawned`] hands it to that isolate's event loop.
pub struct AsyncJob {
    pub resolve: crate::Global,
    pub reject: crate::Global,
    pub future: core::pin::Pin<Box<dyn core::future::Future<Output = Settle> + Send>>,
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
