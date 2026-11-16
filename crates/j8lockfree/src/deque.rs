//! Chase–Lev 工作窃取双端队列（Lê 等 2013 年的 C11 内存序版本）：主人在底端压 / 弹，别人从顶端偷。
//!
//! - 槽是 `AtomicUsize`：元素是一个字（`T: Copy`，与 `usize` 互转），偷的人读槽不构成数据竞争
//! - 扩容：主人换一块两倍大的缓冲，旧缓冲压进 [`PushList`] 留到析构时才释放（偷的人可能还在读）
//! - 主人唯一：[`Deque::owner`] 用原子位发一个主人凭据，凭据析构时交回
use alloc::boxed::Box;
use core::cell::Cell;
use core::marker::PhantomData;
use core::sync::atomic::{AtomicBool, AtomicIsize, AtomicPtr, AtomicUsize, Ordering, fence};

use crate::PushList;

/// 初始容量（2 的幂）
const INITIAL: usize = 64;

/// 一块环形缓冲，容量是 2 的幂。
struct Buffer {
    slots: Box<[AtomicUsize]>,
}

impl Buffer {
    fn new(cap: usize) -> Self {
        Self { slots: (0..cap).map(|_| AtomicUsize::new(0)).collect() }
    }

    fn cap(&self) -> usize {
        self.slots.len()
    }

    fn at(&self, i: isize) -> &AtomicUsize {
        // 按位与取模：容量是 2 的幂，负数下标不会出现（top ≥ 0）
        &self.slots[i.cast_unsigned() & (self.cap() - 1)]
    }
}

pub struct Deque<T> {
    top: AtomicIsize,
    bottom: AtomicIsize,
    buf: AtomicPtr<Buffer>,
    /// 换下来的旧缓冲：偷的人可能还在读，析构时一起释放
    retired: PushList<Box<Buffer>>,
    /// 已经发出了主人凭据
    owned: AtomicBool,
    _value: PhantomData<T>,
}

// SAFETY: 槽与指针都是原子量；元素是 Copy 的一个字，跨线程转手要求 T: Send
unsafe impl<T: Send> Sync for Deque<T> {}
// SAFETY: 同上
unsafe impl<T: Send> Send for Deque<T> {}

impl<T> Default for Deque<T> {
    fn default() -> Self {
        Self {
            top: AtomicIsize::new(0),
            bottom: AtomicIsize::new(0),
            buf: AtomicPtr::new(Box::into_raw(Box::new(Buffer::new(INITIAL)))),
            retired: PushList::default(),
            owned: AtomicBool::new(false),
            _value: PhantomData,
        }
    }
}

impl<T: Copy + From<usize>> Deque<T>
where
    usize: From<T>,
{
    /// 领主人凭据（只能有一个）；已经发出去了返回 None。
    pub fn owner(&self) -> Option<Owner<'_, T>> {
        self.owned.compare_exchange(false, true, Ordering::AcqRel, Ordering::Acquire).ok().map(|_| Owner { deque: self, _local: PhantomData })
    }

    /// 从顶端偷一个；空了返回 None（与别人抢输了会重试）。
    pub fn steal(&self) -> Option<T> {
        loop {
            let t = self.top.load(Ordering::Acquire);
            fence(Ordering::SeqCst);
            let b = self.bottom.load(Ordering::Acquire);
            if t >= b {
                return None;
            }
            // SAFETY: 缓冲在 Deque 析构前不会释放（换下来的进 retired）
            let v = unsafe { &*self.buf.load(Ordering::Acquire) }.at(t).load(Ordering::Relaxed);
            if self.top.compare_exchange(t, t + 1, Ordering::SeqCst, Ordering::Relaxed).is_ok() {
                return Some(T::from(v));
            }
        }
    }

    /// 大约还有几个（并发时只是估计）。
    pub fn len(&self) -> usize {
        let (t, b) = (self.top.load(Ordering::Acquire), self.bottom.load(Ordering::Acquire));
        (b - t).max(0).cast_unsigned()
    }

    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }
}

impl<T> Drop for Deque<T> {
    fn drop(&mut self) {
        // SAFETY: 独占；buf 是 Box::into_raw 出来的当前缓冲
        drop(unsafe { Box::from_raw(*self.buf.get_mut()) });
    }
}

/// 主人凭据：底端压 / 弹。只在一个线程上用（不是 `Sync`），析构时交回。
pub struct Owner<'d, T> {
    deque: &'d Deque<T>,
    _local: PhantomData<Cell<()>>,
}

impl<T: Copy + From<usize>> Owner<'_, T>
where
    usize: From<T>,
{
    pub fn push(&self, value: T) {
        let d = self.deque;
        let b = d.bottom.load(Ordering::Relaxed);
        let t = d.top.load(Ordering::Acquire);
        let mut buf = d.buf.load(Ordering::Relaxed);
        // SAFETY: 缓冲在 Deque 析构前不会释放
        if (b - t).cast_unsigned() >= unsafe { &*buf }.cap() {
            buf = self.grow(buf, t, b);
        }
        // SAFETY: 同上
        unsafe { &*buf }.at(b).store(usize::from(value), Ordering::Relaxed);
        fence(Ordering::Release);
        d.bottom.store(b + 1, Ordering::Relaxed);
    }

    pub fn pop(&self) -> Option<T> {
        let d = self.deque;
        let b = d.bottom.load(Ordering::Relaxed) - 1;
        let buf = d.buf.load(Ordering::Relaxed);
        d.bottom.store(b, Ordering::Relaxed);
        fence(Ordering::SeqCst);
        let t = d.top.load(Ordering::Relaxed);
        if t > b {
            d.bottom.store(b + 1, Ordering::Relaxed);
            return None;
        }
        // SAFETY: 缓冲在 Deque 析构前不会释放
        let v = unsafe { &*buf }.at(b).load(Ordering::Relaxed);
        if t == b {
            // 最后一个：和偷的人抢
            let won = d.top.compare_exchange(t, t + 1, Ordering::SeqCst, Ordering::Relaxed).is_ok();
            d.bottom.store(b + 1, Ordering::Relaxed);
            return won.then(|| T::from(v));
        }
        Some(T::from(v))
    }

    /// 换一块两倍大的缓冲，把 `[t, b)` 抄过去；旧的留到析构
    #[cold]
    fn grow(&self, old: *mut Buffer, t: isize, b: isize) -> *mut Buffer {
        // SAFETY: 主人独占换缓冲；旧缓冲在析构前不释放
        let from = unsafe { &*old };
        let to = Buffer::new(from.cap() * 2);
        for i in t..b {
            to.at(i).store(from.at(i).load(Ordering::Relaxed), Ordering::Relaxed);
        }
        let to = Box::into_raw(Box::new(to));
        self.deque.buf.store(to, Ordering::Release);
        // SAFETY: old 是 Box::into_raw 出来的，从此只由 retired 持有（偷的人可能还在读，析构时才释放）
        let _ = self.deque.retired.push(unsafe { Box::from_raw(old) });
        to
    }
}

impl<T> Drop for Owner<'_, T> {
    fn drop(&mut self) {
        self.deque.owned.store(false, Ordering::Release);
    }
}
