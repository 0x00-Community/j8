//! `Lock` / `RefLock`：放在 `Gc` 里的可变单元，写的时候经写屏障。
use core::cell::{BorrowError, BorrowMutError, Cell, Ref, RefCell, RefMut};
use core::fmt;

use crate::{Collect, Gc, Mutation, Relocate, Trace};

/// `Cell` 的回收版：`Gc<Lock<T>>::set` 要 `&Mutation`。
#[derive(Default)]
pub struct Lock<T>(Cell<T>);

impl<T> Lock<T> {
    pub fn new(t: T) -> Self {
        Self(Cell::new(t))
    }

    pub fn into_inner(self) -> T {
        self.0.into_inner()
    }

    pub fn get_mut(&mut self) -> &mut T {
        self.0.get_mut()
    }
}

impl<T: Copy> Lock<T> {
    pub fn get(&self) -> T {
        self.0.get()
    }
}

impl<T> From<T> for Lock<T> {
    fn from(t: T) -> Self {
        Self::new(t)
    }
}

impl<T: Copy + fmt::Debug> fmt::Debug for Lock<T> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_tuple("Lock").field(&self.get()).finish()
    }
}

impl<'gc, T: Copy + 'gc> Gc<'gc, Lock<T>> {
    pub fn get(self) -> T {
        self.as_ref().get()
    }

    pub fn set(self, mc: &Mutation<'gc>, t: T) {
        mc.cx().barrier(self.erase());
        self.as_ref().0.set(t);
    }
}

unsafe impl<'gc, T: Collect<'gc> + Copy + 'gc> Collect<'gc> for Lock<T> {
    const NEEDS_TRACE: bool = T::NEEDS_TRACE;
    const PARALLEL_DROP: bool = T::PARALLEL_DROP;

    fn trace(&self, t: &mut Trace<'_>) {
        t.trace(&self.get());
    }

    fn relocate(&mut self, r: &mut Relocate<'_>) {
        r.relocate(self.get_mut());
    }
}

/// `RefCell` 的回收版：`Gc<RefLock<T>>::borrow_mut` 要 `&Mutation`。
#[derive(Default)]
pub struct RefLock<T>(RefCell<T>);

impl<T> RefLock<T> {
    pub fn new(t: T) -> Self {
        Self(RefCell::new(t))
    }

    pub fn into_inner(self) -> T {
        self.0.into_inner()
    }

    pub fn get_mut(&mut self) -> &mut T {
        self.0.get_mut()
    }

    pub fn borrow(&self) -> Ref<'_, T> {
        self.0.borrow()
    }

    pub fn try_borrow(&self) -> Result<Ref<'_, T>, BorrowError> {
        self.0.try_borrow()
    }
}

impl<T> From<T> for RefLock<T> {
    fn from(t: T) -> Self {
        Self::new(t)
    }
}

impl<T: fmt::Debug> fmt::Debug for RefLock<T> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        fmt::Debug::fmt(&self.0, f)
    }
}

impl<'gc, T: 'gc> Gc<'gc, RefLock<T>> {
    pub fn borrow(self) -> Ref<'gc, T> {
        self.as_ref().0.borrow()
    }

    pub fn try_borrow(self) -> Result<Ref<'gc, T>, BorrowError> {
        self.as_ref().0.try_borrow()
    }

    pub fn borrow_mut(self, mc: &Mutation<'gc>) -> RefMut<'gc, T> {
        mc.cx().barrier(self.erase());
        self.as_ref().0.borrow_mut()
    }

    pub fn try_borrow_mut(self, mc: &Mutation<'gc>) -> Result<RefMut<'gc, T>, BorrowMutError> {
        mc.cx().barrier(self.erase());
        self.as_ref().0.try_borrow_mut()
    }
}

unsafe impl<'gc, T: Collect<'gc> + 'gc> Collect<'gc> for RefLock<T> {
    const NEEDS_TRACE: bool = T::NEEDS_TRACE;
    const PARALLEL_DROP: bool = T::PARALLEL_DROP;

    fn trace(&self, t: &mut Trace<'_>) {
        // SAFETY: 回收只在 `mutate` 之外，没有借出中的 RefMut
        t.trace(unsafe { &*self.0.as_ptr() });
    }

    fn relocate(&mut self, r: &mut Relocate<'_>) {
        r.relocate(self.get_mut());
    }
}
