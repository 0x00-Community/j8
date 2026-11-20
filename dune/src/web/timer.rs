//! One pending timer: `new Timer().wait(ms)` is a promise for `true` once `ms` have passed, or `false` once the timer
//! is cancelled (so a cleared timer does not keep the event loop waiting).
use std::future::Future;
use std::pin::Pin;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::task::{Context, Poll};
use std::time::Duration;

use j8::frame::{Agent, Error, Value};
use j8::Glue;

use crate::host::Offload;

#[derive(Default)]
pub struct Timer {
    cancelled: Arc<AtomicBool>,
}

#[j8::class]
impl Timer {
    #[js(constructor)]
    fn new() -> Self {
        Self::default()
    }

    fn wait<'gc>(&self, a: &mut Agent<'gc, '_>, ms: f64) -> Result<Value<'gc>, Error> {
        let sleep = Sleep { cancelled: self.cancelled.clone(), slept: Offload::new(move || std::thread::sleep(Duration::from_secs_f64(ms.max(0.0) / 1e3))) };
        Glue::spawn(a, async move { Glue::settle(sleep.await) })
    }

    /// Cancelling happens on the runtime thread, which polls every pending job again before it parks, so the
    /// waiting promise settles without a wake-up
    fn cancel(&self) {
        self.cancelled.store(true, Ordering::Release);
    }
}

/// Sleeps on a thread of its own; `false` as soon as it is cancelled
struct Sleep<F> {
    cancelled: Arc<AtomicBool>,
    slept: Offload<(), F>,
}

impl<F: FnOnce() + Send + 'static> Future for Sleep<F> {
    type Output = bool;

    fn poll(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<bool> {
        if self.cancelled.load(Ordering::Acquire) {
            return Poll::Ready(false);
        }
        Pin::new(&mut self.slept).poll(cx).map(|()| true)
    }
}
