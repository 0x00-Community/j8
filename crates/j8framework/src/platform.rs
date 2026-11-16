use core::time::Duration;

/// A background task handed to the platform to run (borrows live for `'t`).
pub type Task<'t> = j8gc::Job<'t>;

/// Threads and clocks provided by the embedder. The thread pool is the supertrait [`j8gc::Pool`] (the GC also runs in
/// parallel on it).
pub trait Platform: Sync + j8gc::Pool {
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
