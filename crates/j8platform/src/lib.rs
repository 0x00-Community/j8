use std::num::NonZeroUsize;
use std::time::{Duration, Instant};

use moremath::{Executor, ThreadMulti};
use j8framework::{Platform, Task};

pub struct DefaultPlatform {
    pool: ThreadMulti,
    start: Instant,
}

impl Default for DefaultPlatform {
    fn default() -> Self {
        Self { pool: ThreadMulti::default(), start: Instant::now() }
    }
}

impl From<NonZeroUsize> for DefaultPlatform {
    fn from(n: NonZeroUsize) -> Self {
        Self { pool: ThreadMulti::from(n), start: Instant::now() }
    }
}

impl j8framework::Pool for DefaultPlatform {
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
