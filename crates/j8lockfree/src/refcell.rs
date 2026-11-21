use core::cell::UnsafeCell;
use core::fmt;
use core::ops::{Deref, DerefMut};
use core::sync::atomic::{AtomicUsize, Ordering};

const WRITING: usize = 1 << (usize::BITS - 1);

pub struct AtomicRefCell<T: ?Sized> {
    state: AtomicUsize,
    value: UnsafeCell<T>,
}

unsafe impl<T: ?Sized + Send + Sync> Sync for AtomicRefCell<T> {}

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
                Ok(_) => return Some(AtomicRef { state: &self.state, value: unsafe { &*self.value.get() } }),
                Err(now) => s = now,
            }
        }
    }

    pub fn try_borrow_mut(&self) -> Option<AtomicRefMut<'_, T>> {
        self.state.compare_exchange(0, WRITING, Ordering::Acquire, Ordering::Relaxed).ok()?;
        Some(AtomicRefMut { state: &self.state, value: unsafe { &mut *self.value.get() } })
    }

    pub fn borrow(&self) -> AtomicRef<'_, T> {
        self.try_borrow().expect("AtomicRefCell already mutably borrowed")
    }

    pub fn borrow_mut(&self) -> AtomicRefMut<'_, T> {
        self.try_borrow_mut().expect("AtomicRefCell already borrowed")
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
