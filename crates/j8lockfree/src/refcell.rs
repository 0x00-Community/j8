//! [`AtomicRefCell`]：可跨线程的 `RefCell`。借用冲突时像 `RefCell` 一样返回错误，从不等待。
use core::cell::UnsafeCell;
use core::fmt;
use core::ops::{Deref, DerefMut};
use core::sync::atomic::{AtomicUsize, Ordering};

/// 最高位：有写者；其余位：读者数
const WRITING: usize = 1 << (usize::BITS - 1);

pub struct AtomicRefCell<T: ?Sized> {
    state: AtomicUsize,
    value: UnsafeCell<T>,
}

// SAFETY: 借用标志保证同一时刻要么一个写者、要么只有读者
unsafe impl<T: ?Sized + Send + Sync> Sync for AtomicRefCell<T> {}
// SAFETY: 拥有它的值
unsafe impl<T: ?Sized + Send> Send for AtomicRefCell<T> {}

impl<T> AtomicRefCell<T> {
    pub fn new(t: T) -> Self {
        Self { state: AtomicUsize::new(0), value: UnsafeCell::new(t) }
    }

    pub fn into_inner(self) -> T {
        self.value.into_inner()
    }
}

impl<T: ?Sized> AtomicRefCell<T> {
    pub fn try_borrow(&self) -> Option<AtomicRef<'_, T>> {
        let mut s = self.state.load(Ordering::Relaxed);
        loop {
            if s & WRITING != 0 {
                return None;
            }
            match self.state.compare_exchange_weak(s, s + 1, Ordering::Acquire, Ordering::Relaxed) {
                // SAFETY: 读者计数挂着，没有写者
                Ok(_) => return Some(AtomicRef { state: &self.state, value: unsafe { &*self.value.get() } }),
                Err(now) => s = now,
            }
        }
    }

    pub fn try_borrow_mut(&self) -> Option<AtomicRefMut<'_, T>> {
        self.state.compare_exchange(0, WRITING, Ordering::Acquire, Ordering::Relaxed).ok()?;
        // SAFETY: 独占（WRITING）
        Some(AtomicRefMut { state: &self.state, value: unsafe { &mut *self.value.get() } })
    }

    pub fn borrow(&self) -> AtomicRef<'_, T> {
        self.try_borrow().expect("AtomicRefCell 已被可变借用")
    }

    pub fn borrow_mut(&self) -> AtomicRefMut<'_, T> {
        self.try_borrow_mut().expect("AtomicRefCell 已被借用")
    }

    pub fn get_mut(&mut self) -> &mut T {
        self.value.get_mut()
    }
}

impl<T: Default> Default for AtomicRefCell<T> {
    fn default() -> Self {
        Self::new(T::default())
    }
}

impl<T: ?Sized + fmt::Debug> fmt::Debug for AtomicRefCell<T> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self.try_borrow() {
            Some(r) => f.debug_tuple("AtomicRefCell").field(&&*r).finish(),
            None => f.write_str("AtomicRefCell(<borrowed>)"),
        }
    }
}

pub struct AtomicRef<'c, T: ?Sized> {
    state: &'c AtomicUsize,
    value: &'c T,
}

impl<T: ?Sized> Deref for AtomicRef<'_, T> {
    type Target = T;

    fn deref(&self) -> &T {
        self.value
    }
}

impl<T: ?Sized> Drop for AtomicRef<'_, T> {
    fn drop(&mut self) {
        self.state.fetch_sub(1, Ordering::Release);
    }
}

pub struct AtomicRefMut<'c, T: ?Sized> {
    state: &'c AtomicUsize,
    value: &'c mut T,
}

impl<T: ?Sized> Deref for AtomicRefMut<'_, T> {
    type Target = T;

    fn deref(&self) -> &T {
        self.value
    }
}

impl<T: ?Sized> DerefMut for AtomicRefMut<'_, T> {
    fn deref_mut(&mut self) -> &mut T {
        self.value
    }
}

impl<T: ?Sized> Drop for AtomicRefMut<'_, T> {
    fn drop(&mut self) {
        self.state.store(0, Ordering::Release);
    }
}
