use alloc::sync::Arc;
use core::cell::UnsafeCell;
use core::future::Future;
use core::pin::Pin;
use core::sync::atomic::{AtomicU8, Ordering};
use core::task::{Context, Poll, Waker};

const EMPTY: u8 = 0;
const REGISTERING: u8 = 1;
const WAITING: u8 = 2;
const SENT: u8 = 3;

pub struct Oneshot<T> {
    state: AtomicU8,
    value: UnsafeCell<Option<T>>,
    waker: UnsafeCell<Option<Waker>>,
}

unsafe impl<T: Send> Sync for Oneshot<T> {}

unsafe impl<T: Send> Send for Oneshot<T> {}

impl<T> Oneshot<T> {
    #[allow(clippy::new_ret_no_self)]
    pub fn new() -> (Sender<T>, Receiver<T>) {
        let shared = Arc::new(Self { state: AtomicU8::new(EMPTY), value: UnsafeCell::new(None), waker: UnsafeCell::new(None) });
        (Sender(Arc::clone(&shared)), Receiver(shared))
    }

    fn take(&self) -> T {
        unsafe { (*self.value.get()).take() }.expect("Oneshot polled after it completed")
    }
}

pub struct Sender<T>(Arc<Oneshot<T>>);

impl<T> Sender<T> {
    pub fn send(self, v: T) {
        let o = &*self.0;
        unsafe { *o.value.get() = Some(v) };
        if o.state.swap(SENT, Ordering::AcqRel) == WAITING
            && let Some(w) = unsafe { (*o.waker.get()).take() }
        {
            w.wake();
        }
    }
}

pub struct Receiver<T>(Arc<Oneshot<T>>);

impl<T> Future for Receiver<T> {
    type Output = T;

    fn poll(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<T> {
        let o = &*self.0;
        let seen = o.state.load(Ordering::Acquire);
        if seen == SENT {
            return Poll::Ready(o.take());
        }
        if o.state.compare_exchange(seen, REGISTERING, Ordering::Acquire, Ordering::Acquire).is_err() {
            return Poll::Ready(o.take());
        }
        unsafe { *o.waker.get() = Some(cx.waker().clone()) };
        match o.state.compare_exchange(REGISTERING, WAITING, Ordering::AcqRel, Ordering::Acquire) {
            Ok(_) => Poll::Pending,
            Err(_) => {
                drop(unsafe { (*o.waker.get()).take() });
                Poll::Ready(o.take())
            }
        }
    }
}
