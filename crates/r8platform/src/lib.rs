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

/// 后台任务与 GC 都跑在同一个线程池上。
impl r8gc::Pool for DefaultPlatform {
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

    /// 相对说明符按引用者所在目录解析，规范化后读文件。
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
