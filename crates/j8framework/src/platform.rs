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

pub trait Platform: Sync + Pool {
    fn now(&self) -> Duration;

    fn wall_clock(&self) -> Duration {
        Duration::ZERO
    }

    fn load_module(&self, referrer: &str, specifier: &str) -> Option<(alloc::string::String, alloc::string::String)> {
        let _ = (referrer, specifier);
        None
    }

    fn locale(&self) -> Option<alloc::string::String> {
        None
    }

    fn time_zone(&self) -> Option<alloc::string::String> {
        None
    }

    fn print(&self, line: &str);
}
