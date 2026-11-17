//! [`Prefix`]：前缀共享的定长缓冲区。好几个持有者共用一块，各自只看前 `n` 项（`n` 是自己的长度）；正好站在已写
//! 末尾、且还有空位的持有者可以 CAS 认领下一个位置、原地写入（之后的持有者接着共用这块）；没认领到（分叉）或写满了，
//! 就把自己那段前缀复制到一块两倍大的新缓冲区（像 `Vec` 扩容，复制均摊 O(1)）。写进去的项不再改，读不需要同步。
//! 用来让一条转换链上的隐藏类共用一份键表，不必每个子类都复制父类的全部键。
use alloc::boxed::Box;
use alloc::sync::Arc;
use alloc::vec::Vec;
use core::cell::UnsafeCell;
use core::mem::MaybeUninit;
use core::slice;
use core::sync::atomic::{AtomicUsize, Ordering};

/// 新缓冲区至少这么多位置
const MIN_CAPACITY: usize = 4;

pub struct Prefix<T> {
    slots: Box<[UnsafeCell<MaybeUninit<T>>]>,
    /// 已认领的位置数：`[0, written)` 都写好了（认领与写入在同一次 `extend` 里，返回前写完）
    written: AtomicUsize,
}

// SAFETY: 位置 i 只由认领到它的那一次 extend 写一次，之后只读；读者只读自己长度以内（发布前已写好）的项
unsafe impl<T: Send + Sync> Sync for Prefix<T> {}
// SAFETY: 拥有其中的值
unsafe impl<T: Send> Send for Prefix<T> {}

impl<T: Clone> Prefix<T> {
    /// 装着 `items` 的一块，后面留些空位
    pub fn new(items: Vec<T>) -> Arc<Self> {
        Self::with(items.len(), items)
    }

    /// 至少 `need` 个位置，前 `items.len()` 个写上 `items`
    fn with(need: usize, items: Vec<T>) -> Arc<Self> {
        let capacity = need.max(MIN_CAPACITY).next_power_of_two();
        let written = items.len();
        let slots: Box<[UnsafeCell<MaybeUninit<T>>]> = items.into_iter().map(|t| UnsafeCell::new(MaybeUninit::new(t))).chain((written..capacity).map(|_| UnsafeCell::new(MaybeUninit::uninit()))).collect();
        Arc::new(Self { slots, written: AtomicUsize::new(written) })
    }

    /// 前 `n` 项（调用方保证这 `n` 项是它看见过写好的：它的长度）
    pub fn prefix(&self, n: usize) -> &[T] {
        debug_assert!(n <= self.written.load(Ordering::Acquire), "only written items are read");
        // SAFETY: UnsafeCell<MaybeUninit<T>> 与 T 布局相同；前 n 项都写好了，写好后不再改
        unsafe { slice::from_raw_parts(self.slots.as_ptr().cast::<T>(), n) }
    }

    /// 长 `n` 的持有者加一项：能原地写就原地写、给出同一块；否则给出一块新的（前 `n` 项复制过去，再加这一项）
    pub fn extend(self: &Arc<Self>, n: usize, item: T) -> Arc<Self> {
        if n < self.slots.len() && self.written.compare_exchange(n, n + 1, Ordering::AcqRel, Ordering::Relaxed).is_ok() {
            // SAFETY: 位置 n 刚由这里认领，只有这里写；读者要等新持有者发布后才读它
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
            // SAFETY: 独占；前 written 项写好了，各释放一次
            unsafe { slot.get_mut().assume_init_drop() };
        }
    }
}
