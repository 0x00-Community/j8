use alloc::vec::Vec;
use core::cell::UnsafeCell;
use core::hint;
use core::mem::MaybeUninit;
use core::sync::atomic::{AtomicBool, AtomicUsize, Ordering};

use crate::segments::Segments;

pub(crate) struct Slot<T> {
    pub(crate) full: AtomicBool,
    pub(crate) value: UnsafeCell<MaybeUninit<T>>,
}

impl<T> Default for Slot<T> {
    fn default() -> Self {
        Self { full: AtomicBool::new(false), value: UnsafeCell::new(MaybeUninit::uninit()) }
    }
}

pub struct Fifo<T> {
    head: AtomicUsize,
    tail: AtomicUsize,
    slots: Segments<Slot<T>>,
}

unsafe impl<T: Send> Sync for Fifo<T> {}

unsafe impl<T: Send> Send for Fifo<T> {}

impl<T> Default for Fifo<T> {
    fn default() -> Self {
        Self { head: AtomicUsize::new(0), tail: AtomicUsize::new(0), slots: Segments::default() }
    }
}

impl<T> Fifo<T> {
    pub fn push(&self, value: T) {
        let slot = self.slots.slot(self.tail.fetch_add(1, Ordering::AcqRel));
        unsafe { (*slot.value.get()).write(value) };
        slot.full.store(true, Ordering::Release);
    }

    pub fn pop(&self) -> Option<T> {
        let mut head = self.head.load(Ordering::Acquire);
        loop {
            if head >= self.tail.load(Ordering::Acquire) {
                return None;
            }
            match self.head.compare_exchange_weak(head, head + 1, Ordering::AcqRel, Ordering::Acquire) {
                Ok(_) => break,
                Err(h) => head = h,
            }
        }
        let slot = self.slots.slot(head);
        while !slot.full.load(Ordering::Acquire) {
            hint::spin_loop();
        }
        Some(unsafe { (*slot.value.get()).assume_init_read() })
    }

    pub fn is_empty(&self) -> bool {
        self.head.load(Ordering::Acquire) >= self.tail.load(Ordering::Acquire)
    }

    pub fn compact(&mut self) {
        let rest: Vec<T> = core::iter::from_fn(|| self.pop()).collect();
        self.slots.release();
        *self.head.get_mut() = 0;
        *self.tail.get_mut() = 0;
        for v in rest {
            self.push(v);
        }
    }
}

impl<T> Drop for Fifo<T> {
    fn drop(&mut self) {
        while self.pop().is_some() {}
    }
}
