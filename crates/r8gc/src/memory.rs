//! Where a heap's memory comes from: its own blocks, apart from Rust's allocator (the GC heap and Rust's memory have
//! separate lifetimes: one is reclaimed by reachability, the other freed by ownership).
//!
//! With `std` each heap maps its own chunks (`mmap`, committed on first touch) and carves them into blocks with an
//! atomic cursor; blocks given back have their memory returned to the OS (`madvise`) and wait in a lock-free pool for
//! reuse; large objects are mapped one by one. Nothing is tied to a thread and nothing takes a lock. Without `std` the
//! blocks come from the global allocator.
use alloc::alloc::Layout;
use core::ptr::NonNull;

/// A block address that may cross threads (the memory behind it belongs to whoever holds the block).
#[derive(Clone, Copy)]
pub(crate) struct Block(pub(crate) NonNull<u8>);

// SAFETY: just an address; the memory behind it belongs to whoever claims it
unsafe impl Send for Block {}
// SAFETY: see above
unsafe impl Sync for Block {}

/// Size of one block (young-generation chunks and old-generation pages are one block each; blocks are aligned to it)
pub(crate) const BLOCK: usize = 256 << 10;

#[cfg(feature = "std")]
mod os {
    use alloc::alloc::{Layout, handle_alloc_error};
    use alloc::vec;
    use core::ptr::{self, NonNull};
    use core::sync::atomic::{AtomicUsize, Ordering};

    use r8lockfree::{SegVec, Stock};

    use super::{BLOCK, Block};

    /// Bytes mapped at a time (carved into blocks)
    const CHUNK: usize = 64 << 20;
    /// Blocks in a chunk
    const BLOCKS: usize = CHUNK / BLOCK;
    /// No chunk yet
    const NONE: usize = usize::MAX;
    /// The OS page size large objects are rounded up to
    const OS_PAGE: usize = 4 << 10;

    /// A mapped chunk and how many of its blocks have been handed out (may run past [`BLOCKS`] once it is full).
    struct Chunk {
        base: Block,
        top: AtomicUsize,
    }

    /// One heap's memory.
    pub(crate) struct Memory {
        chunks: SegVec<Chunk>,
        /// Index of the chunk new blocks come from
        current: AtomicUsize,
        /// Blocks given back (their memory already returned to the OS)
        pool: Stock<Block>,
    }

    impl Default for Memory {
        fn default() -> Self {
            Self { chunks: SegVec::default(), current: AtomicUsize::new(NONE), pool: Stock::default() }
        }
    }

    impl Memory {
        /// `bytes` of fresh zeroed memory aligned to `align` (a power of two), committed on first touch
        fn map(bytes: usize, align: usize) -> Option<NonNull<u8>> {
            let extra = align.saturating_sub(OS_PAGE);
            // SAFETY: a fresh anonymous private mapping, touching nothing else
            let m = unsafe { libc::mmap(ptr::null_mut(), bytes + extra, libc::PROT_READ | libc::PROT_WRITE, libc::MAP_PRIVATE | libc::MAP_ANONYMOUS | libc::MAP_NORESERVE, -1, 0) };
            if m == libc::MAP_FAILED {
                return None;
            }
            let start = m as usize;
            let at = start.next_multiple_of(align);
            // SAFETY: trimming the unaligned head and the unused tail of our own mapping
            unsafe {
                if at > start {
                    libc::munmap(m, at - start);
                }
                if start + extra > at {
                    libc::munmap((at + bytes) as *mut libc::c_void, start + extra - at);
                }
            }
            NonNull::new(at as *mut u8)
        }

        /// Unmaps what [`Memory::map`] gave.
        ///
        /// # Safety
        /// `p` is a whole mapping of `bytes` that nothing uses any more.
        unsafe fn unmap(p: NonNull<u8>, bytes: usize) {
            // SAFETY: the caller vouches
            unsafe { libc::munmap(p.as_ptr().cast(), bytes) };
        }

        /// A fresh block (from the pool, the current chunk, or a new chunk)
        pub(crate) fn block(&self) -> NonNull<u8> {
            if let Some(b) = self.pool.claim_one() {
                return b.0;
            }
            loop {
                let i = self.current.load(Ordering::Acquire);
                if let Some(c) = self.chunks.get(i) {
                    let at = c.top.fetch_add(1, Ordering::Relaxed);
                    if at < BLOCKS {
                        // SAFETY: block `at` lies inside the chunk
                        return unsafe { c.base.0.add(at * BLOCK) };
                    }
                }
                // The current chunk is full: map another. If another thread installed one meanwhile, ours stays
                // listed (untouched, so it costs address space only) and is unmapped with the heap
                let base = Self::map(CHUNK, BLOCK).unwrap_or_else(|| handle_alloc_error(super::Memory::LAYOUT));
                let j = self.chunks.push(Chunk { base: Block(base), top: AtomicUsize::new(0) });
                let _ = self.current.compare_exchange(i, j, Ordering::AcqRel, Ordering::Acquire);
            }
        }

        /// Gives a block back: its memory goes back to the OS, the address waits in the pool.
        ///
        /// # Safety
        /// `p` came from [`Memory::block`] of this heap and nothing lives in it any more.
        pub(crate) unsafe fn free_block(&self, p: NonNull<u8>) {
            // SAFETY: the caller vouches the block is unused; the mapping stays
            unsafe { libc::madvise(p.as_ptr().cast(), BLOCK, libc::MADV_DONTNEED) };
            self.pool.give(vec![Block(p)]);
        }

        /// Blocks given back since the last call become reusable (exclusively, during collection)
        pub(crate) fn settle(&mut self) {
            self.pool.settle();
        }

        /// Memory for a large object
        pub(crate) fn alloc(layout: Layout) -> NonNull<u8> {
            Self::map(layout.size().next_multiple_of(OS_PAGE), layout.align().max(OS_PAGE)).unwrap_or_else(|| handle_alloc_error(layout))
        }

        /// Returns a large object's memory.
        ///
        /// # Safety
        /// `p` came from `Memory::alloc(layout)` and is no longer used.
        pub(crate) unsafe fn free(p: NonNull<u8>, layout: Layout) {
            // SAFETY: the caller vouches; `alloc` mapped exactly this many bytes
            unsafe { Self::unmap(p, layout.size().next_multiple_of(OS_PAGE)) };
        }
    }

    impl Drop for Memory {
        fn drop(&mut self) {
            for c in self.chunks.take() {
                // SAFETY: the heap is gone; every chunk was mapped whole by `map`
                unsafe { Self::unmap(c.base.0, CHUNK) };
            }
        }
    }
}

#[cfg(not(feature = "std"))]
mod os {
    use alloc::alloc::Layout;
    use core::ptr::NonNull;

    /// One heap's memory: blocks and large objects from the global allocator.
    #[derive(Default)]
    pub(crate) struct Memory;

    impl Memory {
        pub(crate) fn block(&self) -> NonNull<u8> {
            // SAFETY: the size is not zero
            NonNull::new(unsafe { alloc::alloc::alloc(super::Memory::LAYOUT) }).unwrap_or_else(|| alloc::alloc::handle_alloc_error(super::Memory::LAYOUT))
        }

        /// # Safety
        /// `p` came from [`Memory::block`] and nothing lives in it any more.
        pub(crate) unsafe fn free_block(&self, p: NonNull<u8>) {
            // SAFETY: allocated with LAYOUT
            unsafe { alloc::alloc::dealloc(p.as_ptr(), super::Memory::LAYOUT) };
        }

        pub(crate) fn settle(&mut self) {}

        pub(crate) fn alloc(layout: Layout) -> NonNull<u8> {
            // SAFETY: the size is not zero
            NonNull::new(unsafe { alloc::alloc::alloc(layout) }).unwrap_or_else(|| alloc::alloc::handle_alloc_error(layout))
        }

        /// # Safety
        /// `p` came from `Memory::alloc(layout)` and is no longer used.
        pub(crate) unsafe fn free(p: NonNull<u8>, layout: Layout) {
            // SAFETY: the caller vouches
            unsafe { alloc::alloc::dealloc(p.as_ptr(), layout) };
        }
    }
}

pub(crate) use os::Memory;

impl Memory {
    /// The layout of a block
    pub(crate) const LAYOUT: Layout = match Layout::from_size_align(BLOCK, BLOCK) {
        Ok(l) => l,
        Err(_) => panic!("block layout"),
    };
}
