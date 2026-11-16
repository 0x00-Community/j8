//! `Collect`: how the collector traverses the `Gc`s inside a value.
use alloc::boxed::Box;
use alloc::collections::{BTreeMap, BTreeSet, VecDeque};
use alloc::rc::Rc;
use alloc::string::String;
use alloc::vec::Vec;
use core::cell::{Cell, RefCell};
use core::marker::PhantomData;

use crate::header::Ptr;

/// A type that can be placed in a `Gc`.
///
/// # Safety
/// - `trace` must visit every `Gc` in the value, and `relocate` must rewrite every `Gc` (objects get moved)
/// - `trace` / `relocate` touch only this value itself: collection is parallel, and different objects are
///   traversed concurrently on different threads
/// - `Drop` must not touch any `Gc` (they may already be gone when it runs); the derive macro ensures this with
///   `no_drop`
/// - Storing a new `Gc` through interior mutability must go through the write barrier (`Lock` / `RefLock` already
///   do this)
///
/// Usually derived with `#[derive(Collect)]` rather than written by hand.
pub unsafe trait Collect<'gc> {
    /// Set to `false` when the value can never contain a `Gc`; the collector then skips it entirely
    const NEEDS_TRACE: bool = true;

    /// Once dead, the value can be dropped on another thread of the pool (concurrently with other dead objects):
    /// there is no custom `Drop`, and no field's destructor touches thread-affine state or non-atomic state shared
    /// across objects (such as `Rc`).
    /// Defaults to `false`: dropped on the calling thread. The derive macro infers it from the fields.
    const PARALLEL_DROP: bool = false;

    /// Calls [`Trace::trace`] on every `Gc`
    #[inline]
    fn trace(&self, _t: &mut Trace<'_>) {}

    /// Calls [`Relocate::relocate`] on every `Gc` (fields are updated in place to the moved addresses)
    #[inline]
    fn relocate(&mut self, _r: &mut Relocate<'_>) {}
}

/// A worker that sees an edge during marking (marking / debug verification).
pub(crate) trait Visit {
    fn visit(&mut self, p: Ptr);
}

/// A worker that updates a field while moving objects (promotion / compaction).
pub(crate) trait Mover {
    fn forward(&mut self, p: Ptr) -> Ptr;

    /// Only pointers into the young generation change
    fn young_only(&self) -> bool {
        false
    }
}

/// The visitor used during marking (one per collector thread).
pub struct Trace<'a> {
    visit: &'a mut (dyn Visit + 'a),
}

impl<'a> Trace<'a> {
    pub(crate) fn new(visit: &'a mut (dyn Visit + 'a)) -> Self {
        Self { visit }
    }

    #[inline]
    pub fn trace<'gc, C: Collect<'gc> + ?Sized>(&mut self, value: &C) {
        if C::NEEDS_TRACE {
            value.trace(self);
        }
    }

    #[inline]
    pub(crate) fn ptr(&mut self, p: Ptr) {
        self.visit.visit(p);
    }
}

/// The visitor used while moving objects: updates fields to the new addresses (one per collector thread).
pub struct Relocate<'a> {
    mover: &'a mut (dyn Mover + 'a),
    moves: usize,
}

impl<'a> Relocate<'a> {
    pub(crate) fn new(mover: &'a mut (dyn Mover + 'a)) -> Self {
        Self { mover, moves: 0 }
    }

    #[inline]
    pub fn relocate<'gc, C: Collect<'gc> + ?Sized>(&mut self, value: &mut C) {
        if C::NEEDS_TRACE {
            value.relocate(self);
        }
    }

    /// Number of pointers updated so far. Containers that hash by address use it to decide whether to rebuild their
    /// index.
    pub fn moves(&self) -> usize {
        self.moves
    }

    /// In this pass only pointers into the young generation change (minor collection): containers may skip the parts
    /// where no new pointer has been written since the last minor collection.
    /// Otherwise (compaction) any pointer may change, and everything must be updated.
    pub fn young_only(&self) -> bool {
        self.mover.young_only()
    }

    #[inline]
    pub(crate) fn ptr(&mut self, p: Ptr) -> Ptr {
        let q = self.mover.forward(p);
        if q != p {
            self.moves += 1;
        }
        q
    }
}

/// Types without a `Gc`: nothing to do; dropping just frees memory, so any thread will do
macro_rules! leaf {
    ($($ty:ty),* $(,)?) => {$(
        unsafe impl<'gc> Collect<'gc> for $ty {
            const NEEDS_TRACE: bool = false;
            const PARALLEL_DROP: bool = true;
        }
    )*};
}
leaf!((), bool, char, u8, u16, u32, u64, u128, usize, i8, i16, i32, i64, i128, isize, f32, f64, str, String);

/// `'static` wrappers: cannot contain a `Gc`
macro_rules! leaf_static {
    ($($ty:ident),*) => {$(
        unsafe impl<'gc, T: ?Sized + 'static> Collect<'gc> for $ty<T> {
            const NEEDS_TRACE: bool = false;
        }
    )*};
}
leaf_static!(Rc, Cell, RefCell);

unsafe impl<T: ?Sized> Collect<'_> for PhantomData<T> {
    const NEEDS_TRACE: bool = false;
    const PARALLEL_DROP: bool = true;
}

unsafe impl<T: ?Sized + 'static> Collect<'_> for &'static T {
    const NEEDS_TRACE: bool = false;
    const PARALLEL_DROP: bool = true;
}

/// Containers traversed element by element
macro_rules! sequence {
    ($($ty:ty),*) => {$(
        unsafe impl<'gc, T: Collect<'gc>> Collect<'gc> for $ty {
            const NEEDS_TRACE: bool = T::NEEDS_TRACE;
            const PARALLEL_DROP: bool = T::PARALLEL_DROP;

            #[inline]
            fn trace(&self, t: &mut Trace<'_>) {
                for v in self.iter() { t.trace(v); }
            }

            #[inline]
            fn relocate(&mut self, r: &mut Relocate<'_>) {
                for v in self.iter_mut() { r.relocate(v); }
            }
        }
    )*};
}
sequence!(Option<T>, Vec<T>, VecDeque<T>, [T]);

unsafe impl<'gc, T: Collect<'gc>, const N: usize> Collect<'gc> for [T; N] {
    const NEEDS_TRACE: bool = T::NEEDS_TRACE;
    const PARALLEL_DROP: bool = T::PARALLEL_DROP;

    fn trace(&self, t: &mut Trace<'_>) {
        for v in self {
            t.trace(v);
        }
    }

    fn relocate(&mut self, r: &mut Relocate<'_>) {
        for v in self.iter_mut() {
            r.relocate(v);
        }
    }
}

unsafe impl<'gc, T: Collect<'gc> + ?Sized> Collect<'gc> for Box<T> {
    const NEEDS_TRACE: bool = T::NEEDS_TRACE;
    const PARALLEL_DROP: bool = T::PARALLEL_DROP;

    fn trace(&self, t: &mut Trace<'_>) {
        t.trace(&**self);
    }

    fn relocate(&mut self, r: &mut Relocate<'_>) {
        r.relocate(&mut **self);
    }
}

unsafe impl<'gc, T: Collect<'gc>, E: Collect<'gc>> Collect<'gc> for Result<T, E> {
    const NEEDS_TRACE: bool = T::NEEDS_TRACE || E::NEEDS_TRACE;
    const PARALLEL_DROP: bool = T::PARALLEL_DROP && E::PARALLEL_DROP;

    fn trace(&self, t: &mut Trace<'_>) {
        match self {
            Ok(v) => t.trace(v),
            Err(e) => t.trace(e),
        }
    }

    fn relocate(&mut self, r: &mut Relocate<'_>) {
        match self {
            Ok(v) => r.relocate(v),
            Err(e) => r.relocate(e),
        }
    }
}

/// Keys cannot contain a `Gc` (a changed key requires reordering, which can't be done in place); only values are
/// traversed
unsafe impl<'gc, K: Ord + 'static, V: Collect<'gc>> Collect<'gc> for BTreeMap<K, V> {
    const NEEDS_TRACE: bool = V::NEEDS_TRACE;

    fn trace(&self, t: &mut Trace<'_>) {
        for v in self.values() {
            t.trace(v);
        }
    }

    fn relocate(&mut self, r: &mut Relocate<'_>) {
        for v in self.values_mut() {
            r.relocate(v);
        }
    }
}

unsafe impl<T: Ord + 'static> Collect<'_> for BTreeSet<T> {
    const NEEDS_TRACE: bool = false;
}

/// Tuples
macro_rules! tuple {
    ($(($($n:tt $t:ident),+)),*) => {$(
        unsafe impl<'gc, $($t: Collect<'gc>),+> Collect<'gc> for ($($t,)+) {
            const NEEDS_TRACE: bool = false $(|| $t::NEEDS_TRACE)+;
            const PARALLEL_DROP: bool = true $(&& $t::PARALLEL_DROP)+;

            fn trace(&self, t: &mut Trace<'_>) {
                $(t.trace(&self.$n);)+
            }

            fn relocate(&mut self, r: &mut Relocate<'_>) {
                $(r.relocate(&mut self.$n);)+
            }
        }
    )*};
}
tuple!((0 A), (0 A, 1 B), (0 A, 1 B, 2 C), (0 A, 1 B, 2 C, 3 D), (0 A, 1 B, 2 C, 3 D, 4 E), (0 A, 1 B, 2 C, 3 D, 4 E, 5 F));

/// Used by the derive macro: types that implement `Drop` conflict with it (`#[collect(no_drop)]`)
#[doc(hidden)]
pub trait __MustNotImplDrop {}

#[allow(drop_bounds)]
impl<T: Drop> __MustNotImplDrop for T {}
