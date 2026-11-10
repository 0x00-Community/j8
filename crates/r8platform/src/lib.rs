//! r8platform：[`Platform`] 的标准库实现。
use std::num::NonZeroUsize;
use std::thread;

use r8frame::{Platform, Task};

/// 缺省平台：标准库线程，`run` 用作用域线程按工作线程数分批。
#[derive(Debug)]
pub struct DefaultPlatform {
    workers: NonZeroUsize,
}

impl Default for DefaultPlatform {
    fn default() -> Self {
        Self { workers: thread::available_parallelism().unwrap_or(NonZeroUsize::MIN) }
    }
}

impl Platform for DefaultPlatform {
    fn workers(&self) -> NonZeroUsize {
        self.workers
    }

    fn run(&self, tasks: Vec<Task<'_>>) {
        let mut tasks = tasks.into_iter();
        loop {
            let batch: Vec<Task<'_>> = tasks.by_ref().take(self.workers.get()).collect();
            if batch.is_empty() {
                break;
            }
            thread::scope(|s| batch.into_iter().for_each(|t| drop(s.spawn(t))));
        }
    }
}
