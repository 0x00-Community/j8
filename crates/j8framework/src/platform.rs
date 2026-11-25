use alloc::boxed::Box;
use core::time::Duration;

pub use j8taskqueue::{Job, Pool};

pub type Task<'t> = Job<'t>;

pub type Settle = Box<dyn for<'scope, 'x> FnOnce(&mut crate::Scope<'scope, 'x>) -> Result<crate::Value<'scope>, crate::Error> + Send>;

pub struct AsyncJob<'i> {
    pub resolve: crate::ScopeGlobal<'i, crate::ValueBrand>,
    pub reject: crate::ScopeGlobal<'i, crate::ValueBrand>,
    pub future: core::pin::Pin<Box<dyn core::future::Future<Output = Settle> + Send>>,
}

pub enum TimerRequest<'i> {
    Arm { id: u64, due: Duration, keep_alive: bool, resolve: crate::ScopeGlobal<'i, crate::ValueBrand> },
    Cancel(u64),
}

#[derive(Default)]
pub struct Timers<'i> {
    next: core::sync::atomic::AtomicU64,
    requests: j8lockfree::Fifo<TimerRequest<'i>>,
}

impl<'i> Timers<'i> {
    pub fn arm(&self, due: Duration, keep_alive: bool, resolve: crate::ScopeGlobal<'i, crate::ValueBrand>) -> u64 {
        let id = self.next.fetch_add(1, core::sync::atomic::Ordering::Relaxed);
        self.requests.push(TimerRequest::Arm { id, due, keep_alive, resolve });
        id
    }

    pub fn cancel(&self, id: u64) {
        self.requests.push(TimerRequest::Cancel(id));
    }

    pub fn take(&self) -> alloc::vec::Vec<TimerRequest<'i>> {
        core::iter::from_fn(|| self.requests.pop()).collect()
    }
}

pub trait Platform: Sync + Pool {
    fn now(&self) -> Duration;

    fn wall_clock(&self) -> Duration {
        Duration::ZERO
    }

    fn load_module(&self, referrer: &str, specifier: &str) -> Option<(alloc::string::String, alloc::string::String)> {
        let _ = (referrer, specifier);
        None
    }

    fn print(&self, line: &str);
}
