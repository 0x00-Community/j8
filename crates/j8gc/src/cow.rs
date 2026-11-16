//! Lock-free containers in the heap built on [`Lock`] slots: the collector reclaims old versions and nodes only once
//! nobody references them, so compare-and-swap on a pointer has no ABA problem and nothing needs manual reclamation.
//!
//! - [`CowLock`]: copy-on-write cell for rarely written data: reading is one atomic load of an immutable snapshot;
//!   writing copies, changes the copy and swaps it in (retrying if another thread swapped first)
//! - [`GcQueue`]: Michael–Scott FIFO queue for frequently written queues
use crate::{Collect, Gc, Lock, Mutation, Relocate, Trace};

/// Copy-on-write cell: `Gc<CowLock<T>>::get` gives the current immutable version; `update` installs a new one built
/// from it. The closure passed to `update` may run more than once (when another thread wins the race), so it must
/// not have side effects beyond building the new value.
#[repr(transparent)]
pub struct CowLock<'gc, T: 'gc>(Lock<Gc<'gc, T>>);

impl<'gc, T: Collect<'gc> + 'gc> CowLock<'gc, T> {
    pub fn new(mc: &Mutation<'gc>, t: T) -> Self {
        Self(Lock::new(Gc::new(mc, t)))
    }
}

impl<'gc, T: Collect<'gc> + 'gc> Gc<'gc, CowLock<'gc, T>> {
    /// The current version (immutable; stays valid for the whole `mutate` call)
    pub fn get(self) -> &'gc T {
        self.as_ref().0.get().as_ref()
    }

    /// Replaces the current version with `f(current)`, returning what `f` returned alongside
    pub fn update_with<R>(self, mc: &Mutation<'gc>, mut f: impl FnMut(&T) -> (T, R)) -> R {
        let cell = &self.as_ref().0;
        let mut current = cell.get();
        loop {
            let (next, r) = f(current.as_ref());
            let next = Gc::new(mc, next);
            mc.barrier(self.erase());
            match cell.cas_raw(current, next) {
                Ok(_) => return r,
                Err(now) => current = now,
            }
        }
    }

    /// Replaces the current version with `f(current)`
    pub fn update(self, mc: &Mutation<'gc>, mut f: impl FnMut(&T) -> T) {
        self.update_with(mc, |t| (f(t), ()));
    }

    /// Installs `t` regardless of the current version
    pub fn set(self, mc: &Mutation<'gc>, t: T) {
        let next = Gc::new(mc, t);
        mc.barrier(self.erase());
        let cell = &self.as_ref().0;
        let mut current = cell.get();
        while let Err(now) = cell.cas_raw(current, next) {
            current = now;
        }
    }
}

unsafe impl<'gc, T: Collect<'gc> + 'gc> Collect<'gc> for CowLock<'gc, T> {
    const PARALLEL_DROP: bool = true;

    fn trace(&self, t: &mut Trace<'_>) {
        t.trace(&self.0);
    }

    fn relocate(&mut self, r: &mut Relocate<'_>) {
        r.relocate(&mut self.0);
    }
}

/// A node of [`GcQueue`]: the first one is a dummy without a value.
struct QNode<'gc, T: 'gc> {
    value: Option<T>,
    next: Lock<Option<Gc<'gc, QNode<'gc, T>>>>,
}

unsafe impl<'gc, T: Collect<'gc> + 'gc> Collect<'gc> for QNode<'gc, T> {
    fn trace(&self, t: &mut Trace<'_>) {
        t.trace(&self.value);
        t.trace(&self.next);
    }

    fn relocate(&mut self, r: &mut Relocate<'_>) {
        r.relocate(&mut self.value);
        r.relocate(&mut self.next);
    }
}

/// Michael–Scott lock-free FIFO queue in the heap (`Gc<GcQueue<T>>`): any thread may push and pop at once; popped
/// nodes become garbage.
pub struct GcQueue<'gc, T: 'gc> {
    head: Lock<Gc<'gc, QNode<'gc, T>>>,
    tail: Lock<Gc<'gc, QNode<'gc, T>>>,
}

impl<'gc, T: Collect<'gc> + Clone + 'gc> GcQueue<'gc, T> {
    pub fn new(mc: &Mutation<'gc>) -> Self {
        let dummy = Gc::new(mc, QNode { value: None, next: Lock::new(None) });
        Self { head: Lock::new(dummy), tail: Lock::new(dummy) }
    }
}

impl<'gc, T: Collect<'gc> + Clone + 'gc> Gc<'gc, GcQueue<'gc, T>> {
    /// Moves the tail from `last` to `to` (a barrier on the queue first: it may receive a young node)
    fn swing(self, mc: &Mutation<'gc>, last: Gc<'gc, QNode<'gc, T>>, to: Gc<'gc, QNode<'gc, T>>) {
        mc.barrier(self.erase());
        let _ = self.as_ref().tail.cas_raw(last, to);
    }

    pub fn push(self, mc: &Mutation<'gc>, value: T) {
        let node = Gc::new(mc, QNode { value: Some(value), next: Lock::new(None) });
        let q = self.as_ref();
        loop {
            let last = q.tail.get();
            match last.next.get() {
                None => {
                    mc.barrier(last.erase());
                    if last.next.cas_raw(None, Some(node)).is_ok() {
                        // Swing the tail; someone else may already have
                        self.swing(mc, last, node);
                        return;
                    }
                }
                // The tail lags: help it along
                Some(n) => self.swing(mc, last, n),
            }
        }
    }

    pub fn pop(self, mc: &Mutation<'gc>) -> Option<T> {
        let q = self.as_ref();
        loop {
            let first = q.head.get();
            let last = q.tail.get();
            let next = first.next.get()?;
            if Gc::ptr_eq(first, last) {
                // The tail lags behind a node already linked: help it along
                self.swing(mc, last, next);
                continue;
            }
            let value = next.value.clone();
            mc.barrier(self.erase());
            if q.head.cas_raw(first, next).is_ok() {
                return value;
            }
        }
    }

    /// Nothing queued (a snapshot)
    pub fn is_empty(self) -> bool {
        self.as_ref().head.get().next.get().is_none()
    }
}

unsafe impl<'gc, T: Collect<'gc> + 'gc> Collect<'gc> for GcQueue<'gc, T> {
    fn trace(&self, t: &mut Trace<'_>) {
        t.trace(&self.head);
        t.trace(&self.tail);
    }

    fn relocate(&mut self, r: &mut Relocate<'_>) {
        r.relocate(&mut self.head);
        r.relocate(&mut self.tail);
    }
}
