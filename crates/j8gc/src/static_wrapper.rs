//! `Static<T>`: a `'static` value stored as-is in a collected object (it cannot contain a `Gc`, so it is
//! not traced).
use core::borrow::{Borrow, BorrowMut};
use core::ops::{Deref, DerefMut};

use crate::Collect;

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, PartialOrd, Ord, Hash)]
#[repr(transparent)]
pub struct Static<T: ?Sized>(pub T);

unsafe impl<T: ?Sized + 'static> Collect<'_> for Static<T> {
    const NEEDS_TRACE: bool = false;
}

impl<T> From<T> for Static<T> {
    fn from(t: T) -> Self {
        Self(t)
    }
}

impl<T: ?Sized> Deref for Static<T> {
    type Target = T;

    fn deref(&self) -> &T {
        &self.0
    }
}

impl<T: ?Sized> DerefMut for Static<T> {
    fn deref_mut(&mut self) -> &mut T {
        &mut self.0
    }
}

impl<T: ?Sized> AsRef<T> for Static<T> {
    fn as_ref(&self) -> &T {
        &self.0
    }
}

impl<T: ?Sized> AsMut<T> for Static<T> {
    fn as_mut(&mut self) -> &mut T {
        &mut self.0
    }
}

impl<T: ?Sized> Borrow<T> for Static<T> {
    fn borrow(&self) -> &T {
        &self.0
    }
}

impl<T: ?Sized> BorrowMut<T> for Static<T> {
    fn borrow_mut(&mut self) -> &mut T {
        &mut self.0
    }
}
