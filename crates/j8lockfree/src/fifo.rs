//! 无锁多生产者多消费者先进先出队列。
//!
//! 位置单调递增：生产者 `tail.fetch_add` 占一个位置再写，消费者 CAS 推进 `head` 认领一个位置再读；
//! 槽在 [`Segments`] 里，并发期间只装段，[`Fifo::compact`]（独占）时整理、释放。
use alloc::vec::Vec;
use core::cell::UnsafeCell;
use core::hint;
use core::mem::MaybeUninit;
use core::sync::atomic::{AtomicBool, AtomicUsize, Ordering};

use crate::segments::Segments;

/// 一个槽：写满后 `full` 置位，值只被认领它的消费者读走一次。
pub(crate) struct Slot<T> {
    pub(crate) full: AtomicBool,
    pub(crate) value: UnsafeCell<MaybeUninit<T>>,
}

impl<T> Default for Slot<T> {
    fn default() -> Self {
        Self { full: AtomicBool::new(false), value: UnsafeCell::new(MaybeUninit::uninit()) }
    }
}

/// 无锁先进先出队列。
pub struct Fifo<T> {
    /// 下一个要读的位置
    head: AtomicUsize,
    /// 下一个要写的位置
    tail: AtomicUsize,
    slots: Segments<Slot<T>>,
}

// SAFETY: 值只经 push（写一次）/ pop（认领者读一次）转手，跨线程转手要求 T: Send；其余状态都是原子量
unsafe impl<T: Send> Sync for Fifo<T> {}
// SAFETY: 同上，队列只拥有 T
unsafe impl<T: Send> Send for Fifo<T> {}

impl<T> Default for Fifo<T> {
    fn default() -> Self {
        Self { head: AtomicUsize::new(0), tail: AtomicUsize::new(0), slots: Segments::default() }
    }
}

impl<T> Fifo<T> {
    pub fn push(&self, value: T) {
        let slot = self.slots.slot(self.tail.fetch_add(1, Ordering::AcqRel));
        // SAFETY: 这个位置只分给了这个生产者，写之前没人读（消费者等 full）
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
        // 生产者已占位置、还差一条写入：等它写完
        while !slot.full.load(Ordering::Acquire) {
            hint::spin_loop();
        }
        // SAFETY: 这个位置只认领给了这个消费者，full 之后值已写好，只读一次
        Some(unsafe { (*slot.value.get()).assume_init_read() })
    }

    pub fn is_empty(&self) -> bool {
        self.head.load(Ordering::Acquire) >= self.tail.load(Ordering::Acquire)
    }

    /// 独占时整理：剩下的按次序挪回位置 0 起，释放所有段（并发期间只增不减的段在这里回收）。
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
    /// 没读走的值析构（槽是 MaybeUninit，段释放时不析构值）
    fn drop(&mut self) {
        while self.pop().is_some() {}
    }
}
