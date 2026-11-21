use alloc::boxed::Box;
use core::fmt;
use core::ptr::null_mut;
use core::sync::atomic::{AtomicPtr, Ordering};

pub struct OnceBox<T>(AtomicPtr<T>);

unsafe impl<T: Send + Sync> Sync for OnceBox<T> {}

unsafe impl<T: Send> Send for OnceBox<T> {}

impl<T> Default for OnceBox<T> {
    fn default() -> Self {
        Self(AtomicPtr::new(null_mut()))
    }
}

impl<T> OnceBox<T> {
    pub fn get(&self) -> Option<&T> {
        unsafe { self.0.load(Ordering::Acquire).as_ref() }
    }

    pub fn get_or_init(&self, f: impl FnOnce() -> T) -> &T {
        if let Some(v) = self.get() {
            return v;
        }
        let new = Box::into_raw(Box::new(f()));
        match self.0.compare_exchange(null_mut(), new, Ordering::AcqRel, Ordering::Acquire) {
            Ok(_) => unsafe { &*new },
            Err(won) => {
                drop(unsafe { Box::from_raw(new) });
                unsafe { &*won }
            }
        }
    }

    pub fn get_mut(&mut self) -> Option<&mut T> {
        unsafe { self.0.get_mut().as_mut() }
    }
}

impl<T> Drop for OnceBox<T> {
    fn drop(&mut self) {
        let p = *self.0.get_mut();
        if !p.is_null() {
            drop(unsafe { Box::from_raw(p) });
        }
    }
}

impl<T: Clone> Clone for OnceBox<T> {
    fn clone(&self) -> Self {
        let c = Self::default();
        if let Some(v) = self.get() {
            c.get_or_init(|| v.clone());
        }
        c
    }
}

impl<T: fmt::Debug> fmt::Debug for OnceBox<T> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_tuple("OnceBox").field(&self.get()).finish()
    }
}
