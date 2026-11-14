//! `Gc<'gc, T>`：指向回收堆的指针，品牌 `'gc` 保证出不了 `mutate`。
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

/// 回收堆上的 `T`。`Copy`，零开销；`'gc` 不变，值出不了 [`R8Heap::mutate`](crate::R8Heap::mutate)。
pub struct Gc<'gc, T: 'gc> {
    ptr: NonNull<T>,
    _brand: PhantomData<(&'gc T, Cell<&'gc ()>)>,
}

impl<'gc, T: Collect<'gc> + 'gc> Gc<'gc, T> {
    /// 在堆上放一个值
    #[inline]
    pub fn new(mc: &Mutation<'gc>, t: T) -> Self {
        Self::from(mc.cx().alloc(t))
    }
}

impl<'gc, T: 'gc> Gc<'gc, T> {
    /// 是不是同一个对象（只在一次 `mutate` 里有意义：回收会搬对象）
    #[inline]
    pub fn ptr_eq(a: Self, b: Self) -> bool {
        a.ptr == b.ptr
    }

    /// 对象地址（只在一次 `mutate` 里有意义：回收会搬对象）
    #[inline]
    pub fn as_ptr(gc: Self) -> *const T {
        gc.ptr.as_ptr()
    }

    /// 借出整个 `'gc` 期间（与 gc-arena 同名，按值取 `self`）
    #[inline]
    #[allow(clippy::should_implement_trait)]
    pub fn as_ref(self) -> &'gc T {
        // SAFETY: 品牌保证 'gc 期间不回收，对象活着
        unsafe { self.ptr.as_ref() }
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
    /// 指针本身没有析构
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

/// 比较、哈希都按值（同 `Box`）
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
