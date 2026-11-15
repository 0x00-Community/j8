//! 库存：独占时整理成只读数组，并发时用原子游标成批领取；还回来的整批先挂在 [`PushList`] 上，下次独占时并回去。
//!
//! 数组在两次独占之间不变，领取只是 `fetch_add` 一段下标：没有 ABA、不用回收节点。
use alloc::vec::Vec;
use core::sync::atomic::{AtomicUsize, Ordering};

use crate::PushList;

pub struct Stock<T> {
    items: Vec<T>,
    /// 下一个没领走的下标（并发领取会让它越过 `items.len()`）
    cursor: AtomicUsize,
    /// 并发期间还回来的
    returned: PushList<Vec<T>>,
}

impl<T> Default for Stock<T> {
    fn default() -> Self {
        Self { items: Vec::new(), cursor: AtomicUsize::new(0), returned: PushList::default() }
    }
}

impl<T: Copy> Stock<T> {
    /// 领至多 `n` 个放进 `out`，返回领到几个。
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

    /// 领一个。
    pub fn claim_one(&self) -> Option<T> {
        let mut out = Vec::new();
        (self.claim(1, &mut out) == 1).then(|| out[0])
    }

    /// 并发时还回一批（下次 [`Stock::settle`] 后才能再领到）。
    pub fn give(&self, batch: Vec<T>) {
        if !batch.is_empty() {
            // 库存从不封口，压入不会被退回
            let _ = self.returned.push(batch);
        }
    }

    /// 独占时整理：没领走的加上还回来的，重新排成只读数组。
    pub fn settle(&mut self) {
        let start = core::mem::take(self.cursor.get_mut()).min(self.items.len());
        self.items.drain(..start);
        for batch in self.returned.take() {
            self.items.extend(batch);
        }
    }

    /// 还没领走的个数（并发时只是估计；还回来的未计入）。
    pub fn len(&self) -> usize {
        self.items.len().saturating_sub(self.cursor.load(Ordering::Acquire))
    }

    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }

    /// 独占时加一个（先 settle 过）。
    pub fn push(&mut self, v: T) {
        self.settle();
        self.items.push(v);
    }

    /// 独占时只留下满足 `keep` 的。
    pub fn retain(&mut self, keep: impl FnMut(&T) -> bool) {
        self.settle();
        self.items.retain(keep);
    }

    /// 独占时清空（还回来的也丢掉）。
    pub fn clear(&mut self) {
        self.items.clear();
        *self.cursor.get_mut() = 0;
        drop(self.returned.take());
    }

    /// 独占时全部取出。
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
