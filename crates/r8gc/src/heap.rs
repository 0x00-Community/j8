//! The heap: young generation (per-block bump allocation), old generation (paged by size class, each page cut into
//! slots at once + free lists), large objects (allocated separately).
use alloc::alloc::{Layout, alloc, dealloc, handle_alloc_error};
use alloc::vec::Vec;
use core::ptr::NonNull;

use crate::header::{Flags, HEADER, Ptr, SizeClass, VTable};
use crate::parallel::{Pool, Spin, split};

/// Size of one young-generation block
pub(crate) const CHUNK: usize = 256 << 10;
/// A minor collection runs once the young generation uses this much
pub(crate) const NURSERY_LIMIT: usize = 4 << 20;
/// Size of one old-generation page (pages are aligned to it)
const PAGE: usize = 256 << 10;
/// A page is evacuated when its live fraction falls below this (denominator)
const SPARSE: usize = 4;
/// Don't compact if less than this much space would be freed
const COMPACT_MIN: usize = 1 << 20;
/// The freed space must also reach this fraction (denominator) of the old generation before compacting:
/// compaction has to update pointers across the whole heap
const COMPACT_FRACTION: usize = 8;
/// Number of slots a promotion buffer takes at a time
const LAB_SLOTS: usize = 64;
/// Empty pages are kept for reuse: keep at least this many (returning them to the system and getting them back
/// costs mmap and page faults)
const SPARE_MIN: usize = 64;
/// Keep at most this fraction (denominator) of the live page count as empty pages
const SPARE_RATIO: usize = 4;

/// Requests a block of `layout` from the global allocator; reports an allocation error on failure
fn raw(layout: Layout) -> NonNull<u8> {
    // SAFETY: sizes are never zero
    NonNull::new(unsafe { alloc(layout) }).unwrap_or_else(|| handle_alloc_error(layout))
}

/// The young generation: a list of blocks, with bump allocation in the current block (the cursor lives in
/// `Context`).
#[derive(Default)]
pub(crate) struct Nursery {
    /// Blocks in use; the last one is the current block
    chunks: Vec<NonNull<u8>>,
    /// Blocks kept for reuse after collection
    spare: Vec<NonNull<u8>>,
    /// Young objects to drop: those not moved during a minor collection are dead
    pub(crate) drops: Vec<Ptr>,
}

impl Nursery {
    const LAYOUT: Layout = match Layout::from_size_align(CHUNK, HEADER) {
        Ok(l) => l,
        Err(_) => panic!("chunk layout"),
    };

    /// Switches to a new block and returns its `[start, end)`
    pub(crate) fn refill(&mut self) -> (usize, usize) {
        let c = self.spare.pop().unwrap_or_else(|| raw(Self::LAYOUT));
        self.chunks.push(c);
        let start = c.as_ptr() as usize;
        (start, start + CHUNK)
    }

    /// Bytes used (`cursor` is the current block's cursor)
    pub(crate) fn used(&self, cursor: usize) -> usize {
        match self.chunks.last() {
            Some(c) => (self.chunks.len() - 1) * CHUNK + (cursor - c.as_ptr() as usize),
            None => 0,
        }
    }

    /// Clears everything: some blocks are kept for reuse
    pub(crate) fn reset(&mut self) {
        self.spare.append(&mut self.chunks);
        while self.spare.len() > NURSERY_LIMIT / CHUNK {
            let c = self.spare.pop().expect("spare chunk");
            // SAFETY: allocated with LAYOUT
            unsafe { dealloc(c.as_ptr(), Self::LAYOUT) }
        }
    }
}

impl Drop for Nursery {
    fn drop(&mut self) {
        for c in self.chunks.drain(..).chain(self.spare.drain(..)) {
            // SAFETY: allocated with LAYOUT
            unsafe { dealloc(c.as_ptr(), Self::LAYOUT) }
        }
    }
}

/// An old-generation page: filled with slots of one size class; free slots are marked `FREE` in their header.
struct Page {
    base: NonNull<u8>,
    class: usize,
    /// Number of live slots after the last sweep
    live: usize,
    /// To be evacuated during compaction
    evacuate: bool,
}

// SAFETY: the page is the sole owner of this memory
unsafe impl Send for Page {}

impl Page {
    const LAYOUT: Layout = match Layout::from_size_align(PAGE, PAGE) {
        Ok(l) => l,
        Err(_) => panic!("page layout"),
    };

    fn slot(&self) -> usize {
        SizeClass::SLOTS[self.class]
    }

    /// The slots in the page (value pointers)
    fn slots(&self) -> impl Iterator<Item = Ptr> + use<> {
        let (base, slot) = (self.base, self.slot());
        (0..PAGE / slot).map(move |i| {
            // SAFETY: i * slot < PAGE
            Ptr::at_slot(unsafe { base.add(i * slot) })
        })
    }

    #[cfg(feature = "verify")]
    fn contains(&self, p: Ptr) -> bool {
        p.addr() & !(PAGE - 1) == self.base.as_ptr() as usize
    }

    /// Sweeps a page: unmarked objects are dropped (in place if they can be dropped on this thread) and reclaimed;
    /// the rest are handed back to the calling thread. Returns the free slots and the objects to hand back
    fn sweep(&mut self, marked: impl Fn(u32) -> bool, parallel_drop: bool) -> (Vec<Ptr>, Vec<Ptr>) {
        let (mut free, mut dead) = (Vec::new(), Vec::new());
        self.live = 0;
        for p in self.slots() {
            let h = p.header();
            let f = h.flags();
            if f & Flags::FREE != 0 {
                free.push(p);
            } else if marked(f) {
                self.live += 1;
            } else if p.vtable().drop.is_some() && !(parallel_drop && p.vtable().parallel_drop) {
                dead.push(p);
            } else {
                // SAFETY: unmarked ⇒ unreachable, dropped only this once; the type allows dropping on this thread
                unsafe { p.drop_value() };
                h.free();
                free.push(p);
            }
        }
        (free, dead)
    }
}

/// A large object.
struct Large {
    base: NonNull<u8>,
    layout: Layout,
    ptr: Ptr,
}

/// The old generation.
pub(crate) struct OldSpace {
    pages: Vec<Page>,
    /// Emptied pages kept for reuse
    spare: Vec<NonNull<u8>>,
    /// Free slots per size class
    free: [Vec<Ptr>; SizeClass::LARGE],
    large: Vec<Large>,
    /// Bytes occupied (live slots + large objects)
    pub(crate) bytes: usize,
}

// SAFETY: it owns the memory of the pages and large objects; protected by a spin lock during collection
unsafe impl Send for OldSpace {}

impl Default for OldSpace {
    fn default() -> Self {
        Self { pages: Vec::new(), spare: Vec::new(), free: core::array::from_fn(|_| Vec::new()), large: Vec::new(), bytes: 0 }
    }
}

impl OldSpace {
    /// Takes up to `n` free slots of class `class` into `out` (opens a page if there are none, preferring kept empty
    /// pages)
    pub(crate) fn take(&mut self, class: usize, n: usize, out: &mut Vec<Ptr>) {
        if self.free[class].is_empty() {
            let base = self.new_page(class);
            let free = &mut self.free[class];
            free.extend(Page { base, class, live: 0, evacuate: false }.slots().inspect(|p| p.header().free()));
        }
        let free = &mut self.free[class];
        out.extend(free.drain(free.len().saturating_sub(n)..));
    }

    /// Opens a page (preferring kept empty pages) and registers it; slot headers are written by whoever takes them
    fn new_page(&mut self, class: usize) -> NonNull<u8> {
        let base = self.spare.pop().unwrap_or_else(|| raw(Page::LAYOUT));
        self.pages.push(Page { base, class, live: 0, evacuate: false });
        base
    }

    /// For a promotion buffer: takes a batch from the free list if available, otherwise hands it a whole page
    /// (returning the page's `[start, end)`)
    fn refill(&mut self, class: usize, out: &mut Vec<Ptr>) -> Option<(usize, usize)> {
        if self.free[class].is_empty() {
            let start = self.new_page(class).as_ptr() as usize;
            let slot = SizeClass::SLOTS[class];
            return Some((start, start + PAGE / slot * slot));
        }
        self.take(class, LAB_SLOTS, out);
        None
    }

    /// Empty pages are kept for reuse and returned to the system only when there are too many
    fn retire(&mut self, base: NonNull<u8>) {
        if self.spare.len() < SPARE_MIN.max(self.pages.len() / SPARE_RATIO) {
            self.spare.push(base);
        } else {
            // SAFETY: allocated with Page::LAYOUT, and no live objects remain in it
            unsafe { dealloc(base.as_ptr(), Page::LAYOUT) }
        }
    }

    /// Returns unused free slots
    pub(crate) fn give(&mut self, class: usize, slots: &mut Vec<Ptr>) {
        self.free[class].append(slots);
    }

    /// Allocates a large object (header not written)
    pub(crate) fn alloc_large(&mut self, vt: &VTable) -> Ptr {
        let offset = vt.align.max(HEADER);
        let layout = Layout::from_size_align(offset + vt.size, offset).expect("large object layout");
        let base = raw(layout);
        // SAFETY: offset < layout.size()
        let ptr = Ptr::from(unsafe { base.add(offset) });
        self.bytes += layout.size();
        self.large.push(Large { base, layout, ptr });
        ptr
    }

    /// Sweeps (pages in parallel): unmarked objects are reclaimed, those needing drop are dropped on the calling
    /// thread; empty pages go back to the allocator. Returns the live bytes
    pub(crate) fn sweep(&mut self, pool: &dyn Pool, epoch: bool, parallel_drop: bool) -> usize {
        let marked = move |f: u32| (f & Flags::MARK != 0) == epoch;
        let swept = split(pool, &mut self.pages, &|pages: &mut [Page]| pages.iter_mut().map(|p| p.sweep(marked, parallel_drop)).collect::<Vec<_>>());
        self.free.iter_mut().for_each(Vec::clear);
        let mut bytes = 0;
        let mut kept = Vec::with_capacity(self.pages.len());
        let pages = core::mem::take(&mut self.pages);
        for (page, (mut free, dead)) in pages.into_iter().zip(swept.into_iter().flatten()) {
            for p in dead {
                // SAFETY: unmarked ⇒ unreachable, dropped only this once; the value's type may not be droppable
                // across threads, so it is done here
                unsafe { p.drop_value() };
                p.header().free();
                free.push(p);
            }
            if page.live == 0 {
                self.retire(page.base);
                continue;
            }
            bytes += page.live * page.slot();
            self.free[page.class].append(&mut free);
            kept.push(page);
        }
        self.pages = kept;
        self.large.retain(|l| {
            if marked(l.ptr.header().flags()) {
                bytes += l.layout.size();
                return true;
            }
            // SAFETY: unreachable; dropped once, then freed
            unsafe {
                l.ptr.drop_value();
                dealloc(l.base.as_ptr(), l.layout);
            }
            false
        });
        self.bytes = bytes;
        bytes
    }

    /// Selects the sparse pages to evacuate (only when worthwhile) and removes their free slots from the free lists.
    /// Returns the objects to move and their size classes
    pub(crate) fn select_evacuation(&mut self) -> Vec<(Ptr, usize)> {
        let mut reclaim = 0;
        for p in &mut self.pages {
            if p.live * p.slot() * SPARSE < PAGE {
                p.evacuate = true;
                reclaim += PAGE - p.live * p.slot();
            }
        }
        if reclaim < COMPACT_MIN.max(self.bytes / COMPACT_FRACTION) {
            self.pages.iter_mut().for_each(|p| p.evacuate = false);
            return Vec::new();
        }
        let mut evacuating: Vec<usize> = self.pages.iter().filter(|p| p.evacuate).map(|p| p.base.as_ptr() as usize).collect();
        evacuating.sort_unstable();
        for free in &mut self.free {
            free.retain(|s| evacuating.binary_search(&(s.addr() & !(PAGE - 1))).is_err());
        }
        let pages = &self.pages;
        pages.iter().filter(|p| p.evacuate).flat_map(|p| p.slots().map(move |s| (s, p.class))).filter(|(s, _)| s.header().flags() & Flags::FREE == 0).collect()
    }

    /// Frees the evacuated pages
    pub(crate) fn release_evacuated(&mut self, moved_bytes: usize) {
        let (gone, kept): (Vec<Page>, Vec<Page>) = core::mem::take(&mut self.pages).into_iter().partition(|p| p.evacuate);
        self.pages = kept;
        for p in gone {
            self.retire(p.base);
        }
        // The new slots moved into were already counted once through the promotion buffer
        self.bytes -= moved_bytes;
    }

    /// Every live object in the old generation (excluding pages being evacuated)
    pub(crate) fn live(&self) -> Vec<Ptr> {
        self.pages
            .iter()
            .filter(|p| !p.evacuate)
            .flat_map(Page::slots)
            .filter(|s| s.header().flags() & Flags::FREE == 0)
            .chain(self.large.iter().map(|l| l.ptr))
            .collect()
    }

    /// Debugging: where a pointer lands
    #[cfg(feature = "verify")]
    pub(crate) fn place(&self, p: Ptr) -> &'static str {
        if let Some(g) = self.pages.iter().find(|g| g.contains(p)) {
            return if g.evacuate { "evacuating page" } else { "page" };
        }
        if self.large.iter().any(|l| l.ptr == p) { "large" } else { "unknown" }
    }
}

impl Drop for OldSpace {
    fn drop(&mut self) {
        for p in self.live() {
            // SAFETY: the whole heap is being torn down; each object is dropped once
            unsafe { p.drop_value() }
        }
        for base in self.pages.drain(..).map(|p| p.base).chain(self.spare.drain(..)) {
            // SAFETY: allocated with Page::LAYOUT
            unsafe { dealloc(base.as_ptr(), Page::LAYOUT) }
        }
        for l in self.large.drain(..) {
            // SAFETY: allocated with l.layout
            unsafe { dealloc(l.base.as_ptr(), l.layout) }
        }
    }
}

/// Promotion buffer: free slots a worker uses when placing objects into the old generation, taken from the old
/// generation in batches so that not every slot requires the lock.
pub(crate) struct Lab<'a> {
    old: &'a Spin<OldSpace>,
    /// Slots taken from the free lists, per size class
    slots: [Vec<Ptr>; SizeClass::LARGE],
    /// Unused `[cursor, end)` of the whole page owned exclusively, per size class
    bump: [(usize, usize); SizeClass::LARGE],
    /// Bytes of the slots used
    pub(crate) bytes: usize,
}

impl<'a> Lab<'a> {
    pub(crate) fn new(old: &'a Spin<OldSpace>) -> Self {
        Self { old, slots: core::array::from_fn(|_| Vec::new()), bump: [(0, 0); SizeClass::LARGE], bytes: 0 }
    }

    /// A free slot of class `class` (the header is written by the caller)
    pub(crate) fn slot(&mut self, class: usize) -> Ptr {
        let size = SizeClass::SLOTS[class];
        self.bytes += size;
        loop {
            if let Some(p) = self.slots[class].pop() {
                return p;
            }
            let (at, end) = self.bump[class];
            if at + size <= end {
                self.bump[class].0 = at + size;
                return Ptr::at_slot(NonNull::new(at as *mut u8).expect("page slot"));
            }
            if let Some(page) = self.old.lock().refill(class, &mut self.slots[class]) {
                self.bump[class] = page;
            }
        }
    }
}

impl Drop for Lab<'_> {
    /// Marks unused slots in the owned pages as free and returns them along with other unused slots; used ones are
    /// accounted to the old generation
    fn drop(&mut self) {
        for (class, &(at, end)) in self.bump.iter().enumerate() {
            let size = SizeClass::SLOTS[class];
            self.slots[class]
                .extend((at..end).step_by(size).map(|a| Ptr::at_slot(NonNull::new(a as *mut u8).expect("page slot"))).inspect(|p| p.header().free()));
        }
        let mut old = self.old.lock();
        for (class, slots) in self.slots.iter_mut().enumerate() {
            old.give(class, slots);
        }
        old.bytes += self.bytes;
    }
}
