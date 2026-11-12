use core::time::Duration;

/// 交给平台跑的一件后台任务（借用活到 `'t`）。
pub type Task<'t> = r8gc::Job<'t>;

/// 嵌入方提供的线程与时钟。线程池是父 trait [`r8gc::Pool`]（GC 也在上面并行）。
pub trait Platform: Sync + r8gc::Pool {
    /// 单调时钟：平台建起来以后过了多久
    fn now(&self) -> Duration;

    /// 墙上时钟：Unix 纪元以来过了多久（`Date`）；没有就是纪元本身。
    fn wall_clock(&self) -> Duration {
        Duration::ZERO
    }

    /// 模块加载：`referrer` 里写的 `specifier` ⇒（规范化的路径, 源码）；找不到给 `None`。
    fn load_module(&self, referrer: &str, specifier: &str) -> Option<(alloc::string::String, alloc::string::String)> {
        let _ = (referrer, specifier);
        None
    }

    /// 输出一行（`console.log`）。
    fn print(&self, line: &str);
}
