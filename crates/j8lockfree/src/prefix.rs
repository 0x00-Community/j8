use alloc::boxed::Box;
use alloc::sync::Arc;
use alloc::vec::Vec;
use core::cell::UnsafeCell;
use core::mem::MaybeUninit;
use core::slice;
use core::sync::atomic::{AtomicUsize, Ordering};

const MIN_CAPACITY: usize = 4;

pub struct Prefix<T> {
    slots: Box<[UnsafeCell<MaybeUninit<T>>]>,
    written: AtomicUsize,
}

unsafe impl<T: Send + Sync> Sync for Prefix<T> {}

unsafe impl<T: Send> Send for Prefix<T> {}

impl<T: Clone> Prefix<T> {
    pub fn new(items: Vec<T>) -> Arc<Self> {
        Self::with(items.len(), items)
    }

    fn with(need: usize, items: Vec<T>) -> Arc<Self> {
        let capacity = need.max(MIN_CAPACITY).next_power_of_two();
        let written = items.len();
        let slots: Box<[UnsafeCell<MaybeUninit<T>>]> = items.into_iter().map(|t| UnsafeCell::new(MaybeUninit::new(t))).chain((written..capacity).map(|_| UnsafeCell::new(MaybeUninit::uninit()))).collect();
        Arc::new(Self { slots, written: AtomicUsize::new(written) })
    }

    pub fn prefix(&self, n: usize) -> &[T] {
        debug_assert!(n <= self.written.load(Ordering::Acquire), "only written items are read");
        unsafe { slice::from_raw_parts(self.slots.as_ptr().cast::<T>(), n) }
    }

    pub fn extend(self: &Arc<Self>, n: usize, item: T) -> Arc<Self> {
        if n < self.slots.len() && self.written.compare_exchange(n, n + 1, Ordering::AcqRel, Ordering::Relaxed).is_ok() {
            unsafe { (*self.slots[n].get()).write(item) };
            return Arc::clone(self);
        }
        let mut items = Vec::with_capacity(n + 1);
        items.extend_from_slice(self.prefix(n));
        items.push(item);
        Self::with((n + 1) * 2, items)
    }
}

impl<T> Drop for Prefix<T> {
    fn drop(&mut self) {
        let written = *self.written.get_mut();
        for slot in &mut self.slots[..written] {
            unsafe { slot.get_mut().assume_init_drop() };
        }
    }
}
