//! Object headers and vtables: each object is immediately preceded by a 16-byte header. Headers are atomic: during
//! parallel collection several threads claim and forward objects concurrently.
use core::mem::{align_of, needs_drop, size_of};
use core::ptr::NonNull;
use core::sync::atomic::{AtomicU32, AtomicUsize, Ordering};

use crate::{Collect, Relocate, Trace};

/// Size of the object header, also the maximum object alignment (larger alignments go to large objects)
pub(crate) const HEADER: usize = size_of::<Header>();

/// Flag bits in the header.
pub(crate) struct Flags;

impl Flags {
    /// In the old generation (page or large object); otherwise in the young generation
    pub(crate) const OLD: u32 = 1;
    /// Mark bit: counts as marked only if it equals the current epoch (the epoch flips each major collection, so bits
    /// need not be cleared one by one)
    pub(crate) const MARK: u32 = 1 << 1;
    /// In some gray stack (claimed, not yet traced)
    pub(crate) const GRAY: u32 = 1 << 2;
    /// Old object in the remembered set (may point into the young generation)
    pub(crate) const REMEMBERED: u32 = 1 << 3;
    /// Moved: `meta` is the new address
    pub(crate) const FORWARDED: u32 = 1 << 4;
    /// Large object (allocated separately, never moved)
    pub(crate) const LARGE: u32 = 1 << 5;
    /// Free slot in an old-generation page
    pub(crate) const FREE: u32 = 1 << 6;
    /// Being moved by some thread: other threads wait until it installs the forwarding address
    pub(crate) const BUSY: u32 = 1 << 7;
}

/// Object header.
#[repr(C, align(16))]
pub(crate) struct Header {
    /// Address of the vtable; after the move, the address of the new object
    meta: AtomicUsize,
    flags: AtomicU32,
}

/// Result of claiming a young-generation object before moving it.
pub(crate) enum Claim {
    /// Already in the old generation
    Old,
    /// Already moved
    Forwarded(Ptr),
    /// Ours to move
    Mine,
}

impl Header {
    pub(crate) fn new(vtable: &VTable, flags: u32) -> Self {
        Self { meta: AtomicUsize::new(core::ptr::from_ref(vtable) as usize), flags: AtomicU32::new(flags) }
    }

    #[inline]
    pub(crate) fn flags(&self) -> u32 {
        self.flags.load(Ordering::Relaxed)
    }

    #[inline]
    pub(crate) fn or_flags(&self, f: u32) {
        self.flags.fetch_or(f, Ordering::Relaxed);
    }

    #[inline]
    pub(crate) fn and_flags(&self, f: u32) {
        self.flags.fetch_and(f, Ordering::Relaxed);
    }

    /// Updates the flags with `f` (no change if it returns `None`; retries on CAS failure); returns whether the update
    /// happened
    #[inline]
    pub(crate) fn update(&self, f: impl FnMut(u32) -> Option<u32>) -> bool {
        self.flags.fetch_update(Ordering::AcqRel, Ordering::Acquire, f).is_ok()
    }

    /// Claims before moving: if someone else is moving it, waits until they finish
    pub(crate) fn claim(&self) -> Claim {
        let mut backoff = crate::parallel::Backoff::default();
        loop {
            let f = self.flags.load(Ordering::Acquire);
            if f & Flags::OLD != 0 {
                return Claim::Old;
            }
            if f & Flags::FORWARDED != 0 {
                return Claim::Forwarded(Ptr::from(self.meta.load(Ordering::Acquire)));
            }
            if f & Flags::BUSY != 0 {
                backoff.wait();
                continue;
            }
            if self.flags.compare_exchange_weak(f, f | Flags::BUSY, Ordering::AcqRel, Ordering::Relaxed).is_ok() {
                return Claim::Mine;
            }
        }
    }

    /// Move finished: the header now records the new address
    pub(crate) fn forward(&self, to: Ptr) {
        self.meta.store(to.addr(), Ordering::Release);
        self.flags.store(Flags::FORWARDED, Ordering::Release);
    }

    /// Where it was moved to
    pub(crate) fn forwarded(&self) -> Option<Ptr> {
        (self.flags.load(Ordering::Acquire) & Flags::FORWARDED != 0).then(|| Ptr::from(self.meta.load(Ordering::Acquire)))
    }

    /// Marks it as a free slot
    pub(crate) fn free(&self) {
        self.meta.store(0, Ordering::Relaxed);
        self.flags.store(Flags::FREE, Ordering::Relaxed);
    }
}

/// The collection operations for one type. Promoted per type as a constant; the header only stores its address.
pub(crate) struct VTable {
    /// Size and alignment of the value
    pub(crate) size: usize,
    pub(crate) align: usize,
    /// Header + value, rounded up to 16 bytes
    pub(crate) slot: usize,
    /// Old-generation size class; `SizeClass::LARGE` means a large object
    pub(crate) class: usize,
    pub(crate) needs_trace: bool,
    pub(crate) trace: unsafe fn(Ptr, &mut Trace<'_>),
    pub(crate) relocate: unsafe fn(Ptr, &mut Relocate<'_>),
    pub(crate) drop: Option<unsafe fn(Ptr)>,
    /// Can be dropped on the thread pool once dead
    pub(crate) parallel_drop: bool,
}

impl VTable {
    const fn of<'gc, T: Collect<'gc>>() -> Self {
        let slot = (HEADER + size_of::<T>()).next_multiple_of(HEADER);
        Self {
            size: size_of::<T>(),
            align: align_of::<T>(),
            slot,
            class: if align_of::<T>() > HEADER { SizeClass::LARGE } else { SizeClass::of(slot) },
            needs_trace: T::NEEDS_TRACE,
            trace: Ptr::trace_as::<T>,
            relocate: Ptr::relocate_as::<T>,
            drop: if needs_drop::<T>() { Some(Ptr::drop_as::<T>) } else { None },
            parallel_drop: T::PARALLEL_DROP,
        }
    }
}

/// One vtable per type (a constant, promoted to read-only data when its address is taken).
pub(crate) trait Typed<'gc>: Collect<'gc> + Sized {
    const VTABLE: VTable = VTable::of::<Self>();
}

impl<'gc, T: Collect<'gc>> Typed<'gc> for T {}

/// Old-generation size classes: all slots in a page have the same size.
pub(crate) struct SizeClass;

impl SizeClass {
    /// Slot size of each class (including the header)
    pub(crate) const SLOTS: [usize; 32] = [
        16, 32, 48, 64, 80, 96, 112, 128, 160, 192, 224, 256, 320, 384, 448, 512, 640, 768, 896, 1024, 1280, 1536, 1792, 2048, 2560, 3072, 3584, 4096, 5120,
        6144, 7168, 8192,
    ];
    /// Beyond the largest class: large object
    pub(crate) const LARGE: usize = Self::SLOTS.len();

    const fn of(slot: usize) -> usize {
        let mut i = 0;
        while i < Self::SLOTS.len() {
            if Self::SLOTS[i] >= slot {
                return i;
            }
            i += 1;
        }
        Self::LARGE
    }
}

/// Type-erased object pointer: points at the value (the header is [`HEADER`] bytes before it).
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(crate) struct Ptr(NonNull<u8>);

// SAFETY: just an address. During parallel collection the mutator is stopped, and each object belongs to one
// thread at a time (claimed through the atomic bits in the header)
unsafe impl Send for Ptr {}
unsafe impl Sync for Ptr {}

impl From<usize> for Ptr {
    fn from(addr: usize) -> Self {
        Self(NonNull::new(addr as *mut u8).expect("null gc pointer"))
    }
}

impl<T> From<NonNull<T>> for Ptr {
    fn from(p: NonNull<T>) -> Self {
        Self(p.cast())
    }
}

impl Ptr {
    /// Start of the slot (where the header is) ⇒ the value
    pub(crate) fn at_slot(base: NonNull<u8>) -> Self {
        // SAFETY: a slot is at least as large as a header
        Self(unsafe { base.add(HEADER) })
    }

    #[inline]
    pub(crate) fn addr(self) -> usize {
        self.0.as_ptr() as usize
    }

    #[inline]
    pub(crate) fn as_ptr(self) -> *mut u8 {
        self.0.as_ptr()
    }

    pub(crate) fn cast<T>(self) -> NonNull<T> {
        self.0.cast()
    }

    #[inline]
    pub(crate) fn header<'a>(self) -> &'a Header {
        // SAFETY: every object has a header in front of it, which is alive as long as the object is
        unsafe { self.0.sub(HEADER).cast::<Header>().as_ref() }
    }

    /// The vtable of an object that has not been moved
    #[inline]
    pub(crate) fn vtable<'a>(self) -> &'a VTable {
        debug_assert!(self.header().flags() & (Flags::FORWARDED | Flags::FREE) == 0, "vtable of a moved or freed object {self:?}");
        // SAFETY: while not moved, meta is the address of a `VTable` constant
        unsafe { &*(self.header().meta.load(Ordering::Relaxed) as *const VTable) }
    }

    /// Writes the object header
    ///
    /// # Safety
    /// The preceding [`HEADER`] bytes belong to this slot.
    pub(crate) unsafe fn init(self, vt: &VTable, flags: u32) {
        unsafe { self.0.sub(HEADER).cast::<Header>().write(Header::new(vt, flags)) };
    }

    /// Copies the value into the free slot `to` and writes its header (the value is moved bytewise: every Rust value
    /// can be moved bitwise)
    ///
    /// # Safety
    /// `to` is a free slot of the same size class owned by the caller; `vt` is this object's vtable.
    pub(crate) unsafe fn copy_to(self, vt: &VTable, to: Ptr, flags: u32) {
        unsafe {
            core::ptr::copy_nonoverlapping(self.as_ptr(), to.as_ptr(), vt.size);
            to.init(vt, flags);
        }
    }

    unsafe fn trace_as<'gc, T: Collect<'gc>>(p: Ptr, t: &mut Trace<'_>) {
        unsafe { p.cast::<T>().as_ref() }.trace(t);
    }

    unsafe fn relocate_as<'gc, T: Collect<'gc>>(p: Ptr, r: &mut Relocate<'_>) {
        unsafe { p.cast::<T>().as_mut() }.relocate(r);
    }

    unsafe fn drop_as<'gc, T: Collect<'gc>>(p: Ptr) {
        unsafe { p.cast::<T>().drop_in_place() }
    }

    /// # Safety
    /// The object is alive and not moved, and only the current thread is touching it right now.
    pub(crate) unsafe fn trace(self, t: &mut Trace<'_>) {
        unsafe { (self.vtable().trace)(self, t) }
    }

    /// # Safety
    /// Same as [`Ptr::trace`].
    pub(crate) unsafe fn relocate(self, r: &mut Relocate<'_>) {
        unsafe { (self.vtable().relocate)(self, r) }
    }

    /// # Safety
    /// The object is dead, not moved, and dropped only once.
    pub(crate) unsafe fn drop_value(self) {
        if let Some(d) = self.vtable().drop {
            unsafe { d(self) }
        }
    }
}
