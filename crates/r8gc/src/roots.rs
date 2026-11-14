//! Dynamic roots: store a `Gc` as a handle that lives across `mutate` calls (dropping the handle releases it).
use alloc::rc::Rc;
use alloc::vec::Vec;
use core::cell::{Cell, RefCell};
use core::marker::PhantomData;

use crate::header::Ptr;
use crate::{Collect, Gc, Mutation, Relocate, Root, Rootable, Trace};

/// Slot table shared by the handles.
#[derive(Default)]
struct Slots {
    ptrs: RefCell<Vec<Option<Ptr>>>,
    free: RefCell<Vec<usize>>,
}

/// A set of dynamic roots, stored in the `R8Heap`'s root.
pub struct DynamicRootSet<'gc> {
    slots: Rc<Slots>,
    _brand: PhantomData<Cell<&'gc ()>>,
}

unsafe impl<'gc> Collect<'gc> for DynamicRootSet<'gc> {
    fn trace(&self, t: &mut Trace<'_>) {
        self.slots.ptrs.borrow().iter().flatten().for_each(|&p| t.ptr(p));
    }

    fn relocate(&mut self, r: &mut Relocate<'_>) {
        self.slots.ptrs.borrow_mut().iter_mut().flatten().for_each(|p| *p = r.ptr(*p));
    }
}

impl<'gc> DynamicRootSet<'gc> {
    pub fn new(_mc: &Mutation<'gc>) -> Self {
        Self { slots: Rc::default(), _brand: PhantomData }
    }

    /// Attaches `v` to the root (the root is fully rescanned on every collection, so no write barrier is needed)
    pub fn stash<R>(&self, _mc: &Mutation<'gc>, v: Gc<'gc, Root<'gc, R>>) -> DynamicRoot<R>
    where
        R: for<'a> Rootable<'a>,
        for<'a> Root<'a, R>: Sized,
    {
        let p = Some(v.erase());
        let mut ptrs = self.slots.ptrs.borrow_mut();
        let index = if let Some(i) = self.slots.free.borrow_mut().pop() {
            ptrs[i] = p;
            i
        } else {
            ptrs.push(p);
            ptrs.len() - 1
        };
        DynamicRoot { slots: self.slots.clone(), index, _root: PhantomData }
    }

    /// Fetches it back. The handle must come from this set, otherwise this panics.
    pub fn fetch<R>(&self, root: &DynamicRoot<R>) -> Gc<'gc, Root<'gc, R>>
    where
        R: for<'a> Rootable<'a>,
        for<'a> Root<'a, R>: Sized,
    {
        assert!(Rc::ptr_eq(&self.slots, &root.slots), "DynamicRoot fetched from a different DynamicRootSet");
        Gc::from(self.slots.ptrs.borrow()[root.index].expect("live dynamic root"))
    }

    /// Whether the handle comes from this set
    pub fn contains<R>(&self, root: &DynamicRoot<R>) -> bool
    where
        R: for<'a> Rootable<'a>,
        for<'a> Root<'a, R>: Sized,
    {
        Rc::ptr_eq(&self.slots, &root.slots)
    }
}

/// A root handle that lives across `mutate` calls.
pub struct DynamicRoot<R>
where
    R: for<'a> Rootable<'a>,
    for<'a> Root<'a, R>: Sized,
{
    slots: Rc<Slots>,
    index: usize,
    _root: PhantomData<R>,
}

impl<R> Drop for DynamicRoot<R>
where
    R: for<'a> Rootable<'a>,
    for<'a> Root<'a, R>: Sized,
{
    fn drop(&mut self) {
        self.slots.ptrs.borrow_mut()[self.index] = None;
        self.slots.free.borrow_mut().push(self.index);
    }
}
