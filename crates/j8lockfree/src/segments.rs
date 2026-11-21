use alloc::boxed::Box;
use core::mem;
use core::ptr;
use core::sync::atomic::{AtomicPtr, Ordering};

const BASE: usize = 32;

const SEGMENTS: usize = usize::BITS as usize;

pub(crate) struct Segments<S> {
    table: [AtomicPtr<S>; SEGMENTS],
}

impl<S> Default for Segments<S> {
    fn default() -> Self {
        Self { table: [const { AtomicPtr::new(ptr::null_mut()) }; SEGMENTS] }
    }
}

impl<S: Default> Segments<S> {
    fn locate(pos: usize) -> (usize, usize) {
        let k = (pos / BASE + 1).ilog2() as usize;
        (k, pos - BASE * ((1 << k) - 1))
    }

    fn segment(&self, k: usize) -> *mut S {
        let cur = self.table[k].load(Ordering::Acquire);
        if !cur.is_null() {
            return cur;
        }
        let fresh: Box<[S]> = (0..BASE << k).map(|_| S::default()).collect();
        let fresh = Box::into_raw(fresh).cast::<S>();
        match self.table[k].compare_exchange(ptr::null_mut(), fresh, Ordering::AcqRel, Ordering::Acquire) {
            Ok(_) => fresh,
            Err(won) => {
                drop(unsafe { Box::from_raw(ptr::slice_from_raw_parts_mut(fresh, BASE << k)) });
                won
            }
        }
    }

    pub(crate) fn slot(&self, pos: usize) -> &S {
        let (k, i) = Self::locate(pos);
        unsafe { &*self.segment(k).add(i) }
    }

    pub(crate) fn slot_mut(&mut self, pos: usize) -> Option<&mut S> {
        let (k, i) = Self::locate(pos);
        let p = *self.table[k].get_mut();
        (!p.is_null()).then(|| unsafe { &mut *p.add(i) })
    }

    pub(crate) fn release(&mut self) {
        for (k, seg) in self.table.iter_mut().enumerate() {
            let p = mem::replace(seg.get_mut(), ptr::null_mut());
            if !p.is_null() {
                drop(unsafe { Box::from_raw(ptr::slice_from_raw_parts_mut(p, BASE << k)) });
            }
        }
    }
}

impl<S> Drop for Segments<S> {
    fn drop(&mut self) {
        for (k, seg) in self.table.iter_mut().enumerate() {
            let p = *seg.get_mut();
            if !p.is_null() {
                drop(unsafe { Box::from_raw(ptr::slice_from_raw_parts_mut(p, BASE << k)) });
            }
        }
    }
}
