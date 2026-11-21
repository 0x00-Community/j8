use alloc::vec::Vec;
use core::sync::atomic::{AtomicUsize, Ordering};

use crate::fifo::Slot;
use crate::segments::Segments;

pub struct SegVec<T> {
    len: AtomicUsize,
    slots: Segments<Slot<T>>,
}

unsafe impl<T: Send + Sync> Sync for SegVec<T> {}

unsafe impl<T: Send> Send for SegVec<T> {}

impl<T> Default for SegVec<T> {
    fn default() -> Self {
        Self { len: AtomicUsize::new(0), slots: Segments::default() }
    }
}

impl<T> SegVec<T> {
    pub fn push(&self, value: T) -> usize {
        let i = self.len.fetch_add(1, Ordering::AcqRel);
        let slot = self.slots.slot(i);
        unsafe { (*slot.value.get()).write(value) };
        slot.full.store(true, Ordering::Release);
        i
    }

    pub fn len(&self) -> usize {
        self.len.load(Ordering::Acquire)
    }

    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }

    pub fn get(&self, i: usize) -> Option<&T> {
        if i >= self.len() {
            return None;
        }
        let slot = self.slots.slot(i);
        slot.full.load(Ordering::Acquire).then(|| unsafe { (*slot.value.get()).assume_init_ref() })
    }

    pub fn iter(&self) -> impl Iterator<Item = &T> {
        (0..self.len()).filter_map(|i| self.get(i))
    }

    pub fn iter_mut(&mut self) -> impl Iterator<Item = &mut T> {
        let len = *self.len.get_mut();
        let slots: *mut Segments<Slot<T>> = &raw mut self.slots;
        (0..len).filter_map(move |i| {
            let slot = unsafe { &mut *slots }.slot_mut(i)?;
            slot.full.get_mut().then(|| unsafe { (*slot.value.get()).assume_init_mut() })
        })
    }

    pub fn take(&mut self) -> Vec<T> {
        let len = core::mem::take(self.len.get_mut());
        let out = (0..len)
            .filter_map(|i| {
                let slot = self.slots.slot_mut(i)?;
                core::mem::take(slot.full.get_mut()).then(|| unsafe { (*slot.value.get()).assume_init_read() })
            })
            .collect();
        self.slots.release();
        out
    }
}

impl<T> Extend<T> for SegVec<T> {
    fn extend<I: IntoIterator<Item = T>>(&mut self, iter: I) {
        for v in iter {
            self.push(v);
        }
    }
}

impl<T> FromIterator<T> for SegVec<T> {
    fn from_iter<I: IntoIterator<Item = T>>(iter: I) -> Self {
        let mut v = Self::default();
        v.extend(iter);
        v
    }
}

impl<T> Drop for SegVec<T> {
    fn drop(&mut self) {
        drop(self.take());
    }
}
