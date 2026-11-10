//! r8platform：[`Platform`] 的标准库实现，后台任务跑在 moremath 的 [`ThreadMulti`] 线程池上。
use std::num::NonZeroUsize;
use std::time::{Duration, Instant};

use moremath::{Executor, ThreadMulti};
use r8frame::{Platform, Task};

/// 缺省平台：线程池起一次复用，任务可以借用调用方的数据。
pub struct DefaultPlatform {
    pool: ThreadMulti,
    start: Instant,
}

impl Default for DefaultPlatform {
    /// 每个 CPU 一个工作线程。
    fn default() -> Self {
        Self { pool: ThreadMulti::default(), start: Instant::now() }
    }
}

impl From<NonZeroUsize> for DefaultPlatform {
    /// `n` 个工作线程（含调用线程）。
    fn from(n: NonZeroUsize) -> Self {
        Self { pool: ThreadMulti::from(n), start: Instant::now() }
    }
}

impl Platform for DefaultPlatform {
    fn workers(&self) -> NonZeroUsize {
        NonZeroUsize::new(self.pool.threads()).unwrap_or(NonZeroUsize::MIN)
    }

    fn now(&self) -> Duration {
        self.start.elapsed()
    }

    fn run(&self, tasks: Vec<Task<'_>>) {
        self.pool.execute(tasks);
    }
}
