use alloc::alloc::{Layout, alloc_zeroed, dealloc};
use alloc::boxed::Box;
use alloc::sync::Arc;
use alloc::vec::Vec;
use core::fmt;
use core::ops::{Deref, DerefMut};
use core::ptr::NonNull;
use core::sync::atomic::{AtomicU8, AtomicU16, AtomicU32, AtomicU64, AtomicUsize, Ordering};

const SHARED_ALIGN: usize = 16;

pub enum Backing {
    Owned(Vec<u8>),
    External(External),
    Shared(Arc<SharedMemory>),
}

impl Backing {
    pub fn resize(&mut self, len: usize) -> bool {
        match self {
            Self::Owned(v) => {
                v.resize(len, 0);
                true
            }
            Self::External(e) => e.len == len,
            Self::Shared(s) => s.grow(len),
        }
    }

    pub fn into_vec(self) -> Vec<u8> {
        match self {
            Self::Owned(v) => v,
            other => other.to_vec(),
        }
    }

    pub fn shared(&self) -> Option<&Arc<SharedMemory>> {
        match self {
            Self::Shared(s) => Some(s),
            _ => None,
        }
    }

    pub fn update(&mut self, at: usize, size: usize, f: impl Fn(u64) -> Option<u64>) -> u64 {
        if let Self::Shared(s) = self {
            return s.update(at, size, f);
        }
        let cell = &mut self[at..at + size];
        let mut x = [0u8; 8];
        x[..size].copy_from_slice(cell);
        let old = u64::from_le_bytes(x);
        if let Some(new) = f(old) {
            cell.copy_from_slice(&new.to_le_bytes()[..size]);
        }
        old
    }
}

impl Deref for Backing {
    type Target = [u8];

    fn deref(&self) -> &[u8] {
        match self {
            Self::Owned(v) => v,
            Self::External(e) => e,
            Self::Shared(s) => unsafe { core::slice::from_raw_parts(s.ptr.as_ptr(), s.len()) },
        }
    }
}

impl DerefMut for Backing {
    fn deref_mut(&mut self) -> &mut [u8] {
        match self {
            Self::Owned(v) => v,
            Self::External(e) => e,
            Self::Shared(s) => unsafe { core::slice::from_raw_parts_mut(s.ptr.as_ptr(), s.len()) },
        }
    }
}

impl From<Vec<u8>> for Backing {
    fn from(v: Vec<u8>) -> Self {
        Self::Owned(v)
    }
}

impl From<External> for Backing {
    fn from(e: External) -> Self {
        Self::External(e)
    }
}

impl From<Arc<SharedMemory>> for Backing {
    fn from(s: Arc<SharedMemory>) -> Self {
        Self::Shared(s)
    }
}

impl fmt::Debug for Backing {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let kind = match self {
            Self::Owned(_) => "Owned",
            Self::External(_) => "External",
            Self::Shared(_) => "Shared",
        };
        write!(f, "{kind}({} bytes)", self.len())
    }
}

pub struct External {
    ptr: NonNull<u8>,
    len: usize,
    #[expect(dead_code, reason = "only dropped along with the buffer")]
    owner: Box<dyn Send>,
}

unsafe impl Send for External {}

unsafe impl Sync for External {}

impl External {
    pub fn new<T: AsMut<[u8]> + Send + 'static>(owner: T) -> Self {
        let mut owner = Box::new(owner);
        let bytes = (*owner).as_mut();
        let (ptr, len) = (NonNull::new(bytes.as_mut_ptr()).unwrap_or(NonNull::dangling()), bytes.len());
        Self { ptr, len, owner }
    }

    pub unsafe fn from_raw(ptr: *mut u8, len: usize, owner: Box<dyn Send>) -> Self {
        Self { ptr: NonNull::new(ptr).unwrap_or(NonNull::dangling()), len, owner }
    }
}

impl Deref for External {
    type Target = [u8];

    fn deref(&self) -> &[u8] {
        unsafe { core::slice::from_raw_parts(self.ptr.as_ptr(), self.len) }
    }
}

impl DerefMut for External {
    fn deref_mut(&mut self) -> &mut [u8] {
        unsafe { core::slice::from_raw_parts_mut(self.ptr.as_ptr(), self.len) }
    }
}

pub struct SharedMemory {
    ptr: NonNull<u8>,
    len: AtomicUsize,
    cap: usize,
    max: Option<usize>,
}

unsafe impl Send for SharedMemory {}

unsafe impl Sync for SharedMemory {}

impl SharedMemory {
    pub fn new(len: usize, max: Option<usize>) -> Option<Arc<Self>> {
        let cap = max.unwrap_or(len).max(len);
        let ptr = if cap == 0 {
            NonNull::<u64>::dangling().cast()
        } else {
            let layout = Layout::from_size_align(cap, SHARED_ALIGN).ok()?;
            NonNull::new(unsafe { alloc_zeroed(layout) })?
        };
        Some(Arc::new(Self { ptr, len: AtomicUsize::new(len), cap, max }))
    }

    pub fn len(&self) -> usize {
        self.len.load(Ordering::SeqCst)
    }

    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }

    pub fn max(&self) -> Option<usize> {
        self.max
    }

    pub fn grow(&self, len: usize) -> bool {
        len <= self.cap && self.len.fetch_update(Ordering::SeqCst, Ordering::SeqCst, |cur| (len >= cur).then_some(len)).is_ok()
    }

    fn update(&self, at: usize, size: usize, f: impl Fn(u64) -> Option<u64>) -> u64 {
        assert!(at + size <= self.len() && at.is_multiple_of(size), "atomic access out of bounds or misaligned");
        let p = unsafe { self.ptr.as_ptr().add(at) };
        macro_rules! rmw {
            ($atomic:ty, $int:ty) => {{
                #[allow(clippy::cast_ptr_alignment, reason = "naturally aligned, checked above")]
                let cell = unsafe { <$atomic>::from_ptr(p.cast::<$int>()) };
                let mut old = cell.load(Ordering::SeqCst);
                loop {
                    let Some(new) = f(u64::from(<$int>::from_le(old))) else { return u64::from(<$int>::from_le(old)) };
                    #[allow(clippy::cast_possible_truncation, reason = "an element is only `size` bytes")]
                    let new = (new as $int).to_le();
                    match cell.compare_exchange_weak(old, new, Ordering::SeqCst, Ordering::SeqCst) {
                        Ok(_) => return u64::from(<$int>::from_le(old)),
                        Err(cur) => old = cur,
                    }
                }
            }};
        }
        match size {
            1 => rmw!(AtomicU8, u8),
            2 => rmw!(AtomicU16, u16),
            4 => rmw!(AtomicU32, u32),
            8 => rmw!(AtomicU64, u64),
            _ => unreachable!("integer elements are 1 / 2 / 4 / 8 bytes"),
        }
    }
}

impl Drop for SharedMemory {
    fn drop(&mut self) {
        if self.cap != 0 {
            unsafe { dealloc(self.ptr.as_ptr(), Layout::from_size_align_unchecked(self.cap, SHARED_ALIGN)) }
        }
    }
}

impl fmt::Debug for SharedMemory {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "SharedMemory({} of {} bytes)", self.len(), self.cap)
    }
}
