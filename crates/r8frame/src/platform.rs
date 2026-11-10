use std::num::NonZeroUsize;
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
