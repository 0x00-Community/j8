//! 只追加的分段向量：`len.fetch_add` 占下标、写满置位，下标稳定、可并发读；整理与取出只在独占时做。
use alloc::vec::Vec;
use core::sync::atomic::{AtomicUsize, Ordering};

use crate::fifo::Slot;
use crate::segments::Segments;

pub struct SegVec<T> {
    /// 已占的下标数（其中可能有还没写满的）
    len: AtomicUsize,
    slots: Segments<Slot<T>>,
}

// SAFETY: 值写一次后只读（共享读要求 T: Sync），取出只在独占时；其余是原子量
unsafe impl<T: Send + Sync> Sync for SegVec<T> {}
// SAFETY: 只拥有 T
unsafe impl<T: Send> Send for SegVec<T> {}

impl<T> Default for SegVec<T> {
    fn default() -> Self {
        Self { len: AtomicUsize::new(0), slots: Segments::default() }
    }
}

impl<T> SegVec<T> {
    /// 追加，返回下标。
    pub fn push(&self, value: T) -> usize {
        let i = self.len.fetch_add(1, Ordering::AcqRel);
        let slot = self.slots.slot(i);
        // SAFETY: 下标 i 只分给了这次 push，写满前没人读（读者看 full）
        unsafe { (*slot.value.get()).write(value) };
        slot.full.store(true, Ordering::Release);
        i
    }

    /// 已占的下标数（并发 push 时可能有的还没写满）。
    pub fn len(&self) -> usize {
        self.len.load(Ordering::Acquire)
    }

    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }

    /// 下标 `i` 的值；还没写满或越界返回 None。
    pub fn get(&self, i: usize) -> Option<&T> {
        if i >= self.len() {
            return None;
        }
        let slot = self.slots.slot(i);
        // SAFETY: full 之后值已写好且不再改（取出要独占）
        slot.full.load(Ordering::Acquire).then(|| unsafe { (*slot.value.get()).assume_init_ref() })
    }

    /// 写满了的值，按下标次序。
    pub fn iter(&self) -> impl Iterator<Item = &T> {
        (0..self.len()).filter_map(|i| self.get(i))
    }

    /// 独占时逐个改。
    pub fn iter_mut(&mut self) -> impl Iterator<Item = &mut T> {
        let len = *self.len.get_mut();
        let slots: *mut Segments<Slot<T>> = &raw mut self.slots;
        (0..len).filter_map(move |i| {
            // SAFETY: 独占（&mut self 借给了迭代器）；不同下标是不同的槽，各给出一次
            let slot = unsafe { &mut *slots }.slot_mut(i)?;
            // SAFETY: full 之后值已写好
            slot.full.get_mut().then(|| unsafe { (*slot.value.get()).assume_init_mut() })
        })
    }

    /// 独占时全部取出（按下标次序），自己清空、释放段。
    pub fn take(&mut self) -> Vec<T> {
        let len = core::mem::take(self.len.get_mut());
        let out = (0..len)
            .filter_map(|i| {
                let slot = self.slots.slot_mut(i)?;
                // SAFETY: full 的值只取这一次，随后把 full 清掉
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
