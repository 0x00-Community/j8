use alloc::sync::Arc;
use core::sync::atomic::{AtomicU8, Ordering};

#[derive(Debug, Default)]
#[repr(transparent)]
pub struct Termination(AtomicU8);

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[repr(u8)]
enum State {
    Idle = 0,
    Requested = 1,
    Unwinding = 2,
}

impl Termination {
    pub fn request(&self) {
        let _ = self.0.compare_exchange(State::Idle as u8, State::Requested as u8, Ordering::SeqCst, Ordering::SeqCst);
    }

    pub fn cancel(&self) {
        self.0.store(State::Idle as u8, Ordering::SeqCst);
    }

    pub fn is_set(&self) -> bool {
        self.0.load(Ordering::Relaxed) != State::Idle as u8
    }

    pub fn observe(&self) -> bool {
        if !self.is_set() {
            return false;
        }
        let _ = self.0.compare_exchange(State::Requested as u8, State::Unwinding as u8, Ordering::SeqCst, Ordering::SeqCst);
        true
    }

    pub fn finish(&self) {
        let _ = self.0.compare_exchange(State::Unwinding as u8, State::Idle as u8, Ordering::SeqCst, Ordering::SeqCst);
    }

    pub fn as_ptr(&self) -> *const u8 {
        self.0.as_ptr().cast_const()
    }
}

#[derive(Clone, Debug)]
pub struct IsolateHandle(pub Arc<Termination>);

impl IsolateHandle {
    pub fn terminate(&self) {
        self.0.request();
    }

    pub fn cancel_terminate(&self) {
        self.0.cancel();
    }

    pub fn is_terminating(&self) -> bool {
        self.0.is_set()
    }
}
