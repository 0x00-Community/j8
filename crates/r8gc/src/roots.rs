//! Dynamic roots: store a `Gc` as a handle that lives across `mutate` calls (dropping the handle releases it).
//! Lock-free: any thread may stash, fetch and drop handles at once.
use alloc::sync::Arc;
use core::marker::PhantomData;
use core::sync::atomic::{AtomicUsize, Ordering};

use r8lockfree::{PushList, SegVec};

use crate::header::Ptr;
use crate::{Collect, Gc, Mutation, Relocate, Root, Rootable, Trace};

/// An empty slot's value (no object lives at address 0)
const EMPTY: usize = 0;

/// Slot table shared by the handles: each slot holds an object address, or [`EMPTY`].
#[derive(Default)]
struct Slots {
    ptrs: SegVec<AtomicUsize>,
    /// Indices of emptied slots, for reuse
    free: PushList<usize>,
}

impl Slots {
    /// Every stashed object
    fn live(&self) -> impl Iterator<Item = Ptr> + '_ {
        self.ptrs.iter().map(|a| a.load(Ordering::Acquire)).filter(|&a| a != EMPTY).map(Ptr::from)
    }

    /// A slot for `p`: an emptied one if any, otherwise a new one
    fn stash(&self, p: Ptr) -> usize {
        let mut free = self.free.take();
        let Some(i) = free.next() else { return self.ptrs.push(AtomicUsize::new(p.addr())) };
        for rest in free {
            // Never sealed
            let _ = self.free.push(rest);
        }
        self.ptrs.get(i).expect("freed dynamic root slot").store(p.addr(), Ordering::Release);
        i
    }
}

/// A set of dynamic roots, stored in the `R8Heap`'s root.
pub struct DynamicRootSet<'gc> {
    slots: Arc<Slots>,
    /// Invariant brand that stays `Send` / `Sync`
    _brand: PhantomData<fn(&'gc ()) -> &'gc ()>,
}

unsafe impl<'gc> Collect<'gc> for DynamicRootSet<'gc> {
    fn trace(&self, t: &mut Trace<'_>) {
        self.slots.live().for_each(|p| t.ptr(p));
    }

    /// Runs only during collection, when no handle is being stashed or fetched
    fn relocate(&mut self, r: &mut Relocate<'_>) {
        for a in self.slots.ptrs.iter() {
            let addr = a.load(Ordering::Acquire);
            if addr != EMPTY {
                a.store(r.ptr(Ptr::from(addr)).addr(), Ordering::Release);
            }
        }
    }
}

impl<'gc> DynamicRootSet<'gc> {
    pub fn new(_mc: &Mutation<'gc>) -> Self {
        Self { slots: Arc::default(), _brand: PhantomData }
    }

    /// Attaches `v` to the root (the root is fully rescanned on every collection, so no write barrier is needed)
    pub fn stash<R>(&self, _mc: &Mutation<'gc>, v: Gc<'gc, Root<'gc, R>>) -> DynamicRoot<R>
    where
        R: for<'a> Rootable<'a>,
        for<'a> Root<'a, R>: Sized,
    {
        DynamicRoot { index: self.slots.stash(v.erase()), slots: self.slots.clone(), _root: PhantomData }
    }

    /// Fetches it back. The handle must come from this set, otherwise this panics.
    pub fn fetch<R>(&self, root: &DynamicRoot<R>) -> Gc<'gc, Root<'gc, R>>
    where
        R: for<'a> Rootable<'a>,
        for<'a> Root<'a, R>: Sized,
    {
        assert!(Arc::ptr_eq(&self.slots, &root.slots), "DynamicRoot fetched from a different DynamicRootSet");
        let addr = self.slots.ptrs.get(root.index).expect("dynamic root slot").load(Ordering::Acquire);
        Gc::from(Ptr::from(addr))
    }

    /// Whether the handle comes from this set
    pub fn contains<R>(&self, root: &DynamicRoot<R>) -> bool
    where
        R: for<'a> Rootable<'a>,
        for<'a> Root<'a, R>: Sized,
    {
        Arc::ptr_eq(&self.slots, &root.slots)
    }
}

/// A root handle that lives across `mutate` calls.
pub struct DynamicRoot<R>
where
    R: for<'a> Rootable<'a>,
    for<'a> Root<'a, R>: Sized,
{
    slots: Arc<Slots>,
    index: usize,
    /// Names the root type without owning one (stays `Send` / `Sync`)
    _root: PhantomData<fn() -> R>,
}

impl<R> Drop for DynamicRoot<R>
where
    R: for<'a> Rootable<'a>,
    for<'a> Root<'a, R>: Sized,
{
    fn drop(&mut self) {
        if let Some(a) = self.slots.ptrs.get(self.index) {
            a.store(EMPTY, Ordering::Release);
            // Never sealed
            let _ = self.slots.free.push(self.index);
        }
    }
}
