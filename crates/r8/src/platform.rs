use std::num::NonZeroUsize;
use std::thread;
use std::time::Instant;

/// 交给平台跑的一件后台任务（借用活到 `'t`）。
pub type Task<'t> = Box<dyn FnOnce() + Send + 't>;

/// 嵌入方提供的线程与时钟。
pub trait Platform: Sync {
    /// 后台工作线程数
    fn workers(&self) -> NonZeroUsize;

    fn now(&self) -> Instant {
        Instant::now()
    }

    /// 跑完一批后台任务再返回（任务可以借用调用方的数据）。
    fn run(&self, tasks: Vec<Task<'_>>);
}

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
