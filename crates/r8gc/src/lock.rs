//! `Lock` / `RefLock`: mutable cells placed inside a `Gc`; writes go through the write barrier. `Lock` is atomic
//! (one word, any thread); `RefLock` is a single-threaded `RefCell` (not `Sync`).
use core::cell::{BorrowError, BorrowMutError, Ref, RefCell, RefMut};
use core::fmt;
use core::marker::PhantomData;
use core::sync::atomic::{AtomicU64, Ordering};

use crate::header::Ptr;
use crate::{Collect, Gc, Mutation, Relocate, Trace};

/// A value that is one machine word with no padding, so a [`Lock`] can keep it in an atomic `u64`.
/// (A trait of its own because `From<u64>` cannot be implemented for `bool` and the like from here.)
pub trait Word: Copy {
    fn into_word(self) -> u64;
    fn from_word(w: u64) -> Self;
}

/// Integers: the word is the value itself (`from_word` only ever sees what `into_word` wrote, so the narrowing
/// casts are exact)
macro_rules! word_int {
    ($($t:ty),*) => {$(
        #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss, clippy::cast_possible_wrap, clippy::cast_lossless)]
        impl Word for $t {
            #[inline]
            fn into_word(self) -> u64 {
                self as u64
            }

            #[inline]
            fn from_word(w: u64) -> Self {
                w as $t
            }
        }
    )*};
}

word_int!(u8, u16, u32, u64, usize, i8, i16, i32, i64, isize);

impl Word for bool {
    #[inline]
    fn into_word(self) -> u64 {
        u64::from(self)
    }

    #[inline]
    fn from_word(w: u64) -> Self {
        w != 0
    }
}

impl Word for f64 {
    #[inline]
    fn into_word(self) -> u64 {
        self.to_bits()
    }

    #[inline]
    fn from_word(w: u64) -> Self {
        f64::from_bits(w)
    }
}

impl<'gc, T: 'gc> Word for Gc<'gc, T> {
    #[inline]
    fn into_word(self) -> u64 {
        self.erase().addr() as u64
    }

    #[allow(clippy::cast_possible_truncation)]
    #[inline]
    fn from_word(w: u64) -> Self {
        Gc::from(Ptr::from(w as usize))
    }
}

/// `None` is the word 0 (no object lives at address 0)
impl<'gc, T: 'gc> Word for Option<Gc<'gc, T>> {
    #[inline]
    fn into_word(self) -> u64 {
        self.map_or(0, Word::into_word)
    }

    #[inline]
    fn from_word(w: u64) -> Self {
        (w != 0).then(|| Gc::from_word(w))
    }
}

/// An atomic cell in the heap: `Gc<Lock<T>>::set` / `swap` / `compare_exchange` require `&Mutation` (they go
/// through the write barrier) and may run on several threads at once. `Lock<Option<Gc<_>>>` is an atomic pointer
/// slot. `Sync` whenever `T` is.
pub struct Lock<T> {
    word: AtomicU64,
    _value: PhantomData<T>,
}

impl<T: Word> Lock<T> {
    pub fn new(t: T) -> Self {
        Self { word: AtomicU64::new(t.into_word()), _value: PhantomData }
    }

    pub fn into_inner(self) -> T {
        T::from_word(self.word.into_inner())
    }

    pub fn get(&self) -> T {
        T::from_word(self.word.load(Ordering::Acquire))
    }

    /// Writes without the barrier: only for a cell nobody else can see yet (`&mut`)
    pub fn set_mut(&mut self, t: T) {
        *self.word.get_mut() = t.into_word();
    }
}

impl<T: Word + Default> Default for Lock<T> {
    fn default() -> Self {
        Self::new(T::default())
    }
}

impl<T: Word> From<T> for Lock<T> {
    fn from(t: T) -> Self {
        Self::new(t)
    }
}

impl<T: Word + fmt::Debug> fmt::Debug for Lock<T> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_tuple("Lock").field(&self.get()).finish()
    }
}

impl<'gc, T: Word + 'gc> Gc<'gc, Lock<T>> {
    pub fn get(self) -> T {
        self.as_ref().get()
    }

    pub fn set(self, mc: &Mutation<'gc>, t: T) {
        mc.barrier(self.erase());
        self.as_ref().word.store(t.into_word(), Ordering::Release);
    }

    /// Stores `t`, returning what was there
    pub fn swap(self, mc: &Mutation<'gc>, t: T) -> T {
        mc.barrier(self.erase());
        T::from_word(self.as_ref().word.swap(t.into_word(), Ordering::AcqRel))
    }

    /// Stores `new` if the cell still holds `current` (compared as words); returns the old value either way
    pub fn compare_exchange(self, mc: &Mutation<'gc>, current: T, new: T) -> Result<T, T> {
        mc.barrier(self.erase());
        self.as_ref().word.compare_exchange(current.into_word(), new.into_word(), Ordering::AcqRel, Ordering::Acquire).map(T::from_word).map_err(T::from_word)
    }
}

unsafe impl<'gc, T: Collect<'gc> + Word + 'gc> Collect<'gc> for Lock<T> {
    const NEEDS_TRACE: bool = T::NEEDS_TRACE;
    /// Just a word
    const PARALLEL_DROP: bool = true;

    fn trace(&self, t: &mut Trace<'_>) {
        t.trace(&self.get());
    }

    fn relocate(&mut self, r: &mut Relocate<'_>) {
        let mut v = T::from_word(*self.word.get_mut());
        r.relocate(&mut v);
        *self.word.get_mut() = v.into_word();
    }
}

/// Collectable version of `RefCell`: `Gc<RefLock<T>>::borrow_mut` requires `&Mutation`.
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
        mc.barrier(self.erase());
        self.as_ref().0.borrow_mut()
    }

    pub fn try_borrow_mut(self, mc: &Mutation<'gc>) -> Result<RefMut<'gc, T>, BorrowMutError> {
        mc.barrier(self.erase());
        self.as_ref().0.try_borrow_mut()
    }
}

unsafe impl<'gc, T: Collect<'gc> + 'gc> Collect<'gc> for RefLock<T> {
    const NEEDS_TRACE: bool = T::NEEDS_TRACE;
    const PARALLEL_DROP: bool = T::PARALLEL_DROP;

    fn trace(&self, t: &mut Trace<'_>) {
        // SAFETY: collection only happens outside `mutate`, so no RefMut is outstanding
        t.trace(unsafe { &*self.0.as_ptr() });
    }

    fn relocate(&mut self, r: &mut Relocate<'_>) {
        r.relocate(self.get_mut());
    }
}
