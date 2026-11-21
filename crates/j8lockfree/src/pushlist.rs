use alloc::boxed::Box;
use alloc::vec::Vec;
use core::ptr;
use core::sync::atomic::{AtomicPtr, Ordering};

struct Node<T> {
    value: T,
    next: *mut Node<T>,
}

pub struct PushList<T> {
    head: AtomicPtr<Node<T>>,
}

unsafe impl<T: Send> Sync for PushList<T> {}

unsafe impl<T: Send> Send for PushList<T> {}

impl<T> Default for PushList<T> {
    fn default() -> Self {
        Self { head: AtomicPtr::new(ptr::null_mut()) }
    }
}

impl<T> PushList<T> {
    const SEALED: *mut Node<T> = ptr::without_provenance_mut(1);

    pub fn push(&self, value: T) -> Result<(), T> {
        let node = Box::into_raw(Box::new(Node { value, next: ptr::null_mut() }));
        let mut head = self.head.load(Ordering::Acquire);
        loop {
            if head == Self::SEALED {
                return Err(unsafe { Box::from_raw(node) }.value);
            }
            unsafe { (*node).next = head };
            match self.head.compare_exchange_weak(head, node, Ordering::AcqRel, Ordering::Acquire) {
                Ok(_) => return Ok(()),
                Err(h) => head = h,
            }
        }
    }

    pub fn take(&self) -> Drain<T> {
        let mut head = self.head.load(Ordering::Acquire);
        loop {
            if head.is_null() || head == Self::SEALED {
                return Drain(ptr::null_mut());
            }
            match self.head.compare_exchange_weak(head, ptr::null_mut(), Ordering::AcqRel, Ordering::Acquire) {
                Ok(_) => return Drain(head),
                Err(h) => head = h,
            }
        }
    }

    pub fn seal(&self) -> Drain<T> {
        let head = self.head.swap(Self::SEALED, Ordering::AcqRel);
        Drain(if head == Self::SEALED { ptr::null_mut() } else { head })
    }

    pub fn is_sealed(&self) -> bool {
        self.head.load(Ordering::Acquire) == Self::SEALED
    }

    pub fn is_empty(&self) -> bool {
        let head = self.head.load(Ordering::Acquire);
        head.is_null() || head == Self::SEALED
    }
}

impl<T> PushList<Vec<T>> {
    pub fn take_concat(&self) -> Vec<T> {
        let mut batches: Vec<Vec<T>> = self.take().collect();
        let Some(big) = batches.iter().enumerate().max_by_key(|(_, b)| b.len()).map(|(i, _)| i) else { return Vec::new() };
        let mut out = batches.swap_remove(big);
        out.reserve(batches.iter().map(Vec::len).sum());
        for b in batches {
            out.extend(b);
        }
        out
    }
}

impl<T> Drop for PushList<T> {
    fn drop(&mut self) {
        let head = *self.head.get_mut();
        drop(Drain(if head == Self::SEALED { ptr::null_mut() } else { head }));
    }
}

pub struct Drain<T>(*mut Node<T>);

unsafe impl<T: Send> Send for Drain<T> {}

impl<T> Iterator for Drain<T> {
    type Item = T;

    fn next(&mut self) -> Option<T> {
        if self.0.is_null() {
            return None;
        }
        let node = unsafe { Box::from_raw(self.0) };
        self.0 = node.next;
        Some(node.value)
    }
}

impl<T> Drop for Drain<T> {
    fn drop(&mut self) {
        for _ in self.by_ref() {}
    }
}
