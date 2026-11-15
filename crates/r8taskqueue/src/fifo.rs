//! 手写的无锁多生产者多消费者先进先出队列。
//!
//! - 位置单调递增：生产者 `tail.fetch_add` 占一个位置再写，消费者 CAS 推进 `head` 认领一个位置再读
//! - 位置按段找槽：第 k 段有 `BASE << k` 个槽，段表定长（段长翻倍，`usize::BITS` 段覆盖全部位置），段按需 CAS 装上
//! - 并发期间只装段、不拆段：没有回收竞争（ABA / 悬垂），段只在独占（`&mut`）时整理、释放
use alloc::boxed::Box;
use alloc::vec::Vec;
use core::cell::UnsafeCell;
use core::hint;
use core::mem::{self, MaybeUninit};
use core::ptr;
use core::sync::atomic::{AtomicBool, AtomicPtr, AtomicUsize, Ordering};

/// 第 0 段的槽数
const BASE: usize = 32;
/// 段表长度：段长翻倍，这么多段覆盖 `usize` 的全部位置
const SEGMENTS: usize = usize::BITS as usize;

/// 一个槽：写满后 `full` 置位，值只被认领它的消费者读走一次。
struct Slot<T> {
    full: AtomicBool,
    value: UnsafeCell<MaybeUninit<T>>,
}

impl<T> Default for Slot<T> {
    fn default() -> Self {
        Self { full: AtomicBool::new(false), value: UnsafeCell::new(MaybeUninit::uninit()) }
    }
}

/// 无锁先进先出队列。
pub(crate) struct Fifo<T> {
    /// 下一个要读的位置
    head: AtomicUsize,
    /// 下一个要写的位置
    tail: AtomicUsize,
    /// 第 k 项指向第 k 段的首槽，没装是空指针
    segments: [AtomicPtr<Slot<T>>; SEGMENTS],
}

// SAFETY: 槽里的值只经 push（写一次）/ pop（认领者读一次）转手，跨线程转手要求 T: Send；其余状态都是原子量
unsafe impl<T: Send> Sync for Fifo<T> {}
// SAFETY: 同上，队列只拥有 T
unsafe impl<T: Send> Send for Fifo<T> {}

impl<T> Default for Fifo<T> {
    fn default() -> Self {
        Self { head: AtomicUsize::new(0), tail: AtomicUsize::new(0), segments: [const { AtomicPtr::new(ptr::null_mut()) }; SEGMENTS] }
    }
}

impl<T> Fifo<T> {
    /// 位置 ⇒（段号，段内下标）：第 k 段从位置 `BASE·(2^k − 1)` 开始
    fn locate(pos: usize) -> (usize, usize) {
        let k = (pos / BASE + 1).ilog2() as usize;
        (k, pos - BASE * ((1 << k) - 1))
    }

    /// 第 k 段的首槽；没装就装上（并发装时输家释放自己那份）
    fn segment(&self, k: usize) -> *mut Slot<T> {
        let cur = self.segments[k].load(Ordering::Acquire);
        if !cur.is_null() {
            return cur;
        }
        let fresh: Box<[Slot<T>]> = (0..BASE << k).map(|_| Slot::default()).collect();
        let fresh = Box::into_raw(fresh).cast::<Slot<T>>();
        match self.segments[k].compare_exchange(ptr::null_mut(), fresh, Ordering::AcqRel, Ordering::Acquire) {
            Ok(_) => fresh,
            Err(won) => {
                // SAFETY: fresh 是刚才 Box::into_raw 出来、长 BASE << k、没给任何人看过的段
                drop(unsafe { Box::from_raw(ptr::slice_from_raw_parts_mut(fresh, BASE << k)) });
                won
            }
        }
    }

    fn slot(&self, pos: usize) -> &Slot<T> {
        let (k, i) = Self::locate(pos);
        // SAFETY: 段装上后在独占整理前一直有效，i < BASE << k
        unsafe { &*self.segment(k).add(i) }
    }

    pub(crate) fn push(&self, value: T) {
        let slot = self.slot(self.tail.fetch_add(1, Ordering::AcqRel));
        // SAFETY: 这个位置只分给了这个生产者，写之前没人读（消费者等 full）
        unsafe { (*slot.value.get()).write(value) };
        slot.full.store(true, Ordering::Release);
    }

    pub(crate) fn pop(&self) -> Option<T> {
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
        let slot = self.slot(head);
        // 生产者已占位置、还差一条写入：等它写完
        while !slot.full.load(Ordering::Acquire) {
            hint::spin_loop();
        }
        // SAFETY: 这个位置只认领给了这个消费者，full 之后值已写好，只读一次
        Some(unsafe { (*slot.value.get()).assume_init_read() })
    }

    pub(crate) fn is_empty(&self) -> bool {
        self.head.load(Ordering::Acquire) >= self.tail.load(Ordering::Acquire)
    }

    /// 独占时整理：剩下的按次序挪回位置 0 起，释放所有段（并发期间只增不减的段在这里回收）。
    pub(crate) fn compact(&mut self) {
        let rest: Vec<T> = core::iter::from_fn(|| self.pop()).collect();
        self.release();
        for v in rest {
            self.push(v);
        }
    }

    /// 释放所有段、位置归零（调用前值都已读走）
    fn release(&mut self) {
        for (k, seg) in self.segments.iter_mut().enumerate() {
            let p = mem::replace(seg.get_mut(), ptr::null_mut());
            if !p.is_null() {
                // SAFETY: 独占；p 是 segment(k) 装上的长 BASE << k 的段，槽里的值都已读走（MaybeUninit 不析构）
                drop(unsafe { Box::from_raw(ptr::slice_from_raw_parts_mut(p, BASE << k)) });
            }
        }
        *self.head.get_mut() = 0;
        *self.tail.get_mut() = 0;
    }
}

impl<T> Drop for Fifo<T> {
    fn drop(&mut self) {
        while self.pop().is_some() {}
        self.release();
    }
}
