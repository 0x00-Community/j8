use alloc::boxed::Box;
use core::time::Duration;

pub use j8taskqueue::{Job, Pool};

pub type Task<'t> = Job<'t>;

pub type Settle = Box<dyn for<'scope, 'x> FnOnce(&mut crate::Agent<'scope, 'x>) -> Result<crate::Value<'scope>, crate::Error> + Send>;

pub struct AsyncJob {
    pub resolve: crate::Global,
    pub reject: crate::Global,
    pub future: core::pin::Pin<Box<dyn core::future::Future<Output = Settle> + Send>>,
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
