//! j8platform: the standard-library implementation of [`Platform`]; background tasks run on moremath's
//! [`ThreadMulti`] thread pool.
use std::num::NonZeroUsize;
use std::time::{Duration, Instant};

use moremath::{Executor, ThreadMulti};
use j8framework::{Platform, Task};

/// Default platform: the thread pool is started once and reused; tasks may borrow the caller's data.
pub struct DefaultPlatform {
    pool: ThreadMulti,
    start: Instant,
}

impl Default for DefaultPlatform {
    /// One worker thread per CPU.
    fn default() -> Self {
        Self { pool: ThreadMulti::default(), start: Instant::now() }
    }
}

impl From<NonZeroUsize> for DefaultPlatform {
    /// `n` worker threads (including the calling thread).
    fn from(n: NonZeroUsize) -> Self {
        Self { pool: ThreadMulti::from(n), start: Instant::now() }
    }
}

/// Background tasks and the GC run on the same thread pool.
impl j8gc::Pool for DefaultPlatform {
    fn threads(&self) -> usize {
        Executor::threads(&self.pool)
    }

    fn run(&self, tasks: Vec<Task<'_>>) {
        self.pool.execute(tasks);
    }
}

impl Platform for DefaultPlatform {
    fn now(&self) -> Duration {
        self.start.elapsed()
    }

    fn wall_clock(&self) -> Duration {
        std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap_or_default()
    }

    /// Relative specifiers resolve against the referrer's directory; the file is read after normalization.
    fn load_module(&self, referrer: &str, specifier: &str) -> Option<(String, String)> {
        let base = std::path::Path::new(referrer).parent().unwrap_or_else(|| std::path::Path::new("."));
        let path = std::fs::canonicalize(base.join(specifier)).ok()?;
        let text = std::fs::read_to_string(&path).ok()?;
        Some((path.display().to_string(), text))
    }

    fn print(&self, line: &str) {
        println!("{line}");
    }
}
