use alloc::boxed::Box;
use core::fmt;
use core::ptr;
use core::sync::atomic::{AtomicPtr, Ordering};

struct Node<T> {
    value: T,
    next: *const Node<T>,
}

pub struct Chain<T> {
    head: AtomicPtr<Node<T>>,
}

unsafe impl<T: Send + Sync> Sync for Chain<T> {}

unsafe impl<T: Send> Send for Chain<T> {}

impl<T> Default for Chain<T> {
    fn default() -> Self {
        Self { head: AtomicPtr::new(ptr::null_mut()) }
    }
}

impl<T> Chain<T> {
    pub fn push(&self, value: T) -> &T {
        let node = Box::into_raw(Box::new(Node { value, next: ptr::null() }));
        let mut cur = self.head.load(Ordering::Acquire);
        loop {
            unsafe { (*node).next = cur };
            match self.head.compare_exchange_weak(cur, node, Ordering::AcqRel, Ordering::Acquire) {
                Ok(_) => return unsafe { &(*node).value },
                Err(now) => cur = now,
            }
        }
    }

    pub fn iter(&self) -> impl Iterator<Item = &T> {
        let mut p: *const Node<T> = self.head.load(Ordering::Acquire);
        core::iter::from_fn(move || {
            let node = unsafe { p.as_ref() }?;
            p = node.next;
            Some(&node.value)
        })
    }

    pub fn is_empty(&self) -> bool {
        self.head.load(Ordering::Acquire).is_null()
    }
}

impl<T> Drop for Chain<T> {
    fn drop(&mut self) {
        let mut p = *self.head.get_mut();
        while !p.is_null() {
            let node = unsafe { Box::from_raw(p) };
            p = node.next.cast_mut();
        }
    }
}

impl<T: fmt::Debug> fmt::Debug for Chain<T> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_list().entries(self.iter()).finish()
    }
}
