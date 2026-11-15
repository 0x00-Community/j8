//! `Gc<'gc, T>`: a pointer into the collected heap; the `'gc` brand guarantees it cannot escape `mutate`.
use core::borrow::Borrow;
use core::cell::Cell;
use core::cmp::Ordering;
use core::fmt;
use core::hash::{Hash, Hasher};
use core::marker::PhantomData;
use core::ops::Deref;
use core::ptr::NonNull;

use crate::header::Ptr;
use crate::{Collect, Mutation, Relocate, Trace};

/// A `T` on the collected heap. `Copy`, zero-cost; `'gc` is invariant, so the value cannot escape
/// [`R8Heap::mutate`](crate::R8Heap::mutate).
pub struct Gc<'gc, T: 'gc> {
    ptr: NonNull<T>,
    _brand: PhantomData<(&'gc T, Cell<&'gc ()>)>,
}

impl<'gc, T: Collect<'gc> + 'gc> Gc<'gc, T> {
    /// Places a value on the heap
    #[inline]
    pub fn new(mc: &Mutation<'gc>, t: T) -> Self {
        Self::from(mc.alloc(t))
    }
}

impl<'gc, T: 'gc> Gc<'gc, T> {
    /// Whether both are the same object (meaningful only within one `mutate`: collection moves objects)
    #[inline]
    pub fn ptr_eq(a: Self, b: Self) -> bool {
        a.ptr == b.ptr
    }

    /// The object's address (meaningful only within one `mutate`: collection moves objects)
    #[inline]
    pub fn as_ptr(gc: Self) -> *const T {
        gc.ptr.as_ptr()
    }

    /// Borrows for the whole `'gc` lifetime (same name as in gc-arena; takes `self` by value)
    #[inline]
    #[allow(clippy::should_implement_trait)]
    pub fn as_ref(self) -> &'gc T {
        // SAFETY: the brand guarantees no collection during 'gc, so the object is alive
        unsafe { self.ptr.as_ref() }
    }

    /// The object at `addr` (the inverse of `Word::into_word`), without checking for null.
    ///
    /// # Safety
    /// `addr` is the address of a live `T` in this heap (as given by `Word::into_word` of a `Gc<'gc, T>`), so not 0.
    #[inline]
    pub unsafe fn from_addr_unchecked(addr: usize) -> Self {
        // SAFETY: the caller vouches it is a live object's address, so not null
        Self { ptr: unsafe { NonNull::new_unchecked(addr as *mut T) }, _brand: PhantomData }
    }

    #[inline]
    pub(crate) fn erase(self) -> Ptr {
        Ptr::from(self.ptr)
    }
}

impl<'gc, T: 'gc> From<Ptr> for Gc<'gc, T> {
    #[inline]
    fn from(p: Ptr) -> Self {
        Self { ptr: p.cast(), _brand: PhantomData }
    }
}

unsafe impl<'gc, T: 'gc> Collect<'gc> for Gc<'gc, T> {
    /// The pointer itself has no destructor
    const PARALLEL_DROP: bool = true;
    #[inline]
    fn trace(&self, t: &mut Trace<'_>) {
        t.ptr(self.erase());
    }

    #[inline]
    fn relocate(&mut self, r: &mut Relocate<'_>) {
        self.ptr = r.ptr(self.erase()).cast();
    }
}

// SAFETY: a `Gc` is a shared reference to its target: it may cross threads when `&T` may
unsafe impl<'gc, T: Sync + 'gc> Send for Gc<'gc, T> {}
// SAFETY: as above
unsafe impl<'gc, T: Sync + 'gc> Sync for Gc<'gc, T> {}

impl<'gc, T: 'gc> Copy for Gc<'gc, T> {}

impl<'gc, T: 'gc> Clone for Gc<'gc, T> {
    fn clone(&self) -> Self {
        *self
    }
}

impl<'gc, T: 'gc> Deref for Gc<'gc, T> {
    type Target = T;

    #[inline]
    fn deref(&self) -> &T {
        Gc::as_ref(*self)
    }
}

impl<'gc, T: 'gc> AsRef<T> for Gc<'gc, T> {
    fn as_ref(&self) -> &T {
        Gc::as_ref(*self)
    }
}

impl<'gc, T: 'gc> Borrow<T> for Gc<'gc, T> {
    fn borrow(&self) -> &T {
        Gc::as_ref(*self)
    }
}

impl<'gc, T: fmt::Debug + 'gc> fmt::Debug for Gc<'gc, T> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        fmt::Debug::fmt(&**self, f)
    }
}

impl<'gc, T: fmt::Display + 'gc> fmt::Display for Gc<'gc, T> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        fmt::Display::fmt(&**self, f)
    }
}

impl<'gc, T: 'gc> fmt::Pointer for Gc<'gc, T> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        fmt::Pointer::fmt(&self.ptr, f)
    }
}

/// Comparison and hashing are by value (like `Box`)
impl<'gc, T: PartialEq + 'gc> PartialEq for Gc<'gc, T> {
    fn eq(&self, o: &Self) -> bool {
        **self == **o
    }
}

impl<'gc, T: Eq + 'gc> Eq for Gc<'gc, T> {}

impl<'gc, T: PartialOrd + 'gc> PartialOrd for Gc<'gc, T> {
    fn partial_cmp(&self, o: &Self) -> Option<Ordering> {
        (**self).partial_cmp(&**o)
    }
}

impl<'gc, T: Ord + 'gc> Ord for Gc<'gc, T> {
    fn cmp(&self, o: &Self) -> Ordering {
        (**self).cmp(&**o)
    }
}

impl<'gc, T: Hash + 'gc> Hash for Gc<'gc, T> {
    fn hash<H: Hasher>(&self, h: &mut H) {
        (**self).hash(h);
    }
}
