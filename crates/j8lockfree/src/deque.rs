use alloc::boxed::Box;
use core::cell::Cell;
use core::marker::PhantomData;
use core::sync::atomic::{AtomicBool, AtomicIsize, AtomicPtr, AtomicUsize, Ordering, fence};

use crate::PushList;

const INITIAL: usize = 64;

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
        &self.slots[i.cast_unsigned() & (self.cap() - 1)]
    }
}

pub struct Deque<T> {
    top: AtomicIsize,
    bottom: AtomicIsize,
    buf: AtomicPtr<Buffer>,
    retired: PushList<Box<Buffer>>,
    owned: AtomicBool,
    _value: PhantomData<T>,
}

unsafe impl<T: Send> Sync for Deque<T> {}

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
    pub fn owner(&self) -> Option<Owner<'_, T>> {
        self.owned.compare_exchange(false, true, Ordering::AcqRel, Ordering::Acquire).ok().map(|_| Owner { deque: self, _local: PhantomData })
    }

    pub fn steal(&self) -> Option<T> {
        loop {
            let t = self.top.load(Ordering::Acquire);
            fence(Ordering::SeqCst);
            let b = self.bottom.load(Ordering::Acquire);
            if t >= b {
                return None;
            }
            let v = unsafe { &*self.buf.load(Ordering::Acquire) }.at(t).load(Ordering::Relaxed);
            if self.top.compare_exchange(t, t + 1, Ordering::SeqCst, Ordering::Relaxed).is_ok() {
                return Some(T::from(v));
            }
        }
    }

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
        drop(unsafe { Box::from_raw(*self.buf.get_mut()) });
    }
}

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
        if (b - t).cast_unsigned() >= unsafe { &*buf }.cap() {
            buf = self.grow(buf, t, b);
        }
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
        let v = unsafe { &*buf }.at(b).load(Ordering::Relaxed);
        if t == b {
            let won = d.top.compare_exchange(t, t + 1, Ordering::SeqCst, Ordering::Relaxed).is_ok();
            d.bottom.store(b + 1, Ordering::Relaxed);
            return won.then(|| T::from(v));
        }
        Some(T::from(v))
    }

    #[cold]
    fn grow(&self, old: *mut Buffer, t: isize, b: isize) -> *mut Buffer {
        let from = unsafe { &*old };
        let to = Buffer::new(from.cap() * 2);
        for i in t..b {
            to.at(i).store(from.at(i).load(Ordering::Relaxed), Ordering::Relaxed);
        }
        let to = Box::into_raw(Box::new(to));
        self.deque.buf.store(to, Ordering::Release);
        let _ = self.deque.retired.push(unsafe { Box::from_raw(old) });
        to
    }
}

impl<T> Drop for Owner<'_, T> {
    fn drop(&mut self) {
        self.deque.owned.store(false, Ordering::Release);
    }
}
