use alloc::vec::Vec;
use core::sync::atomic::{AtomicUsize, Ordering};

use crate::PushList;

pub struct Stock<T> {
    items: Vec<T>,
    cursor: AtomicUsize,
    returned: PushList<Vec<T>>,
}

impl<T> Default for Stock<T> {
    fn default() -> Self {
        Self { items: Vec::new(), cursor: AtomicUsize::new(0), returned: PushList::default() }
    }
}

impl<T: Copy> Stock<T> {
    pub fn claim(&self, n: usize, out: &mut Vec<T>) -> usize {
        let len = self.items.len();
        if self.cursor.load(Ordering::Relaxed) >= len {
            return 0;
        }
        let start = self.cursor.fetch_add(n, Ordering::AcqRel);
        let got = &self.items[start.min(len)..start.saturating_add(n).min(len)];
        out.extend_from_slice(got);
        got.len()
    }

    pub fn claim_one(&self) -> Option<T> {
        let len = self.items.len();
        if self.cursor.load(Ordering::Relaxed) >= len {
            return None;
        }
        self.items.get(self.cursor.fetch_add(1, Ordering::AcqRel)).copied()
    }

    pub fn give(&self, batch: Vec<T>) {
        if !batch.is_empty() {
            let _ = self.returned.push(batch);
        }
    }

    pub fn settle(&mut self) {
        let start = core::mem::take(self.cursor.get_mut()).min(self.items.len());
        self.items.drain(..start);
        for batch in self.returned.take() {
            self.items.extend(batch);
        }
    }

    pub fn len(&self) -> usize {
        self.items.len().saturating_sub(self.cursor.load(Ordering::Acquire))
    }

    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }

    pub fn push(&mut self, v: T) {
        self.settle();
        self.items.push(v);
    }

    pub fn retain(&mut self, keep: impl FnMut(&T) -> bool) {
        self.settle();
        self.items.retain(keep);
    }

    pub fn clear(&mut self) {
        self.items.clear();
        *self.cursor.get_mut() = 0;
        drop(self.returned.take());
    }

    pub fn take(&mut self) -> Vec<T> {
        self.settle();
        core::mem::take(&mut self.items)
    }
}

impl<T: Copy> Extend<T> for Stock<T> {
    fn extend<I: IntoIterator<Item = T>>(&mut self, iter: I) {
        self.settle();
        self.items.extend(iter);
    }
}
