//! 动态根：把 `Gc` 存成跨 `mutate` 的句柄（句柄 drop 即放掉）。
use alloc::rc::Rc;
use alloc::vec::Vec;
use core::cell::{Cell, RefCell};
use core::marker::PhantomData;

use crate::header::Ptr;
use crate::{Collect, Gc, Mutation, Relocate, Root, Rootable, Trace};

/// 句柄共享的槽表。
#[derive(Default)]
struct Slots {
    ptrs: RefCell<Vec<Option<Ptr>>>,
    free: RefCell<Vec<usize>>,
}

/// 一组动态根，放在 `R8Heap` 的根里。
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

    /// 把 `v` 挂到根上（根每次回收都整个重扫，不用写屏障）
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

    /// 取回。句柄必须出自这一组，否则 panic。
    pub fn fetch<R>(&self, root: &DynamicRoot<R>) -> Gc<'gc, Root<'gc, R>>
    where
        R: for<'a> Rootable<'a>,
        for<'a> Root<'a, R>: Sized,
    {
        assert!(Rc::ptr_eq(&self.slots, &root.slots), "DynamicRoot fetched from a different DynamicRootSet");
        Gc::from(self.slots.ptrs.borrow()[root.index].expect("live dynamic root"))
    }

    /// 句柄是不是出自这一组
    pub fn contains<R>(&self, root: &DynamicRoot<R>) -> bool
    where
        R: for<'a> Rootable<'a>,
        for<'a> Root<'a, R>: Sized,
    {
        Rc::ptr_eq(&self.slots, &root.slots)
    }
}

/// 跨 `mutate` 的根句柄。
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
