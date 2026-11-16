// r8alloc: a lock-free allocator over one reserved address range, tied to no thread.
//
// - The range (REGION_BYTES) is reserved once with mmap (MAP_NORESERVE: pages are committed by the first touch).
// - Pages (PAGE bytes, PAGE-aligned) come from an atomic bump cursor; freed runs of n pages go to the list for n
//   (their memory is returned with madvise, the addresses are kept). Runs longer than MAX_RUN pages, and anything
//   once the range is full, are mapped from the OS directly.
// - Small blocks (up to the largest class) come from one global lock-free free list per size class; an empty list
//   takes a page, cuts it into blocks and splices them in with one CAS.
// - There are no per-thread arenas and no locks: any thread may free what any other thread allocated.
//
// Free lists are Treiber stacks whose head is one 64-bit word: a 32-bit version (bumped by every change, so a stale
// CAS fails: no ABA) and a 32-bit link. Page links live in a side table (never in the pages themselves); small-block
// links live in the first 4 bytes of the free block.

// mmap / madvise and their MAP_ / MADV_ flags are POSIX / BSD extensions beyond strict C11
#define _DEFAULT_SOURCE

#include <stdatomic.h>
#include <stddef.h>
#include <stdint.h>
#include <sys/mman.h>

// Bytes of address space reserved
#define REGION_BYTES ((size_t)32 << 30)
// log2 of the page size
#define PAGE_SHIFT 18
#define PAGE ((size_t)1 << PAGE_SHIFT)
// Pages in the range
#define REGION_PAGES (REGION_BYTES >> PAGE_SHIFT)
// Runs up to this many pages are recycled through lists; longer ones go straight to the OS
#define MAX_RUN 16
// Small-block links count 16-byte units from the start of the range (32 GiB / 16 fits in 31 bits)
#define UNIT_SHIFT 4
// Smallest alignment every block has
#define MIN_ALIGN ((size_t)1 << UNIT_SHIFT)
// Number of size classes
#define CLASSES 26
// Width of the link half of a list head
#define LINK_BITS 32
#define LINK_MASK (((uint64_t)1 << LINK_BITS) - 1)

// Block sizes of the classes; the power-of-two ones are also aligned to their size (pages are carved from offset 0)
static const uint32_t CLASS_SIZE[CLASSES] = {
    16,   32,   48,   64,    96,    128,   192,   256,   384,   512,    768,   1024,  1536,
    2048, 3072, 4096, 6144,  8192,  12288, 16384, 24576, 32768, 49152, 65536, 98304, 131072,
};

typedef _Atomic uint64_t list_t;

// Start of the range (0 until the first use reserves it)
static _Atomic uintptr_t region_base;
// Pages handed out from the range so far
static _Atomic uint64_t page_top;
// Freed runs of n pages, n = 1..=MAX_RUN; links are page numbers + 1 (0 ends the list)
static list_t page_free[MAX_RUN + 1];
// Side table: the next page of each freed run
static _Atomic uint32_t page_next[REGION_PAGES];
// Free blocks of each class; links are 16-byte unit numbers + 1 (0 ends the list)
static list_t block_free[CLASSES];

// The range, reserving it on first use (two threads racing both map; the loser unmaps its copy)
static uint8_t *region(void) {
    uintptr_t b = atomic_load_explicit(&region_base, memory_order_acquire);
    if (b != 0) {
        return (uint8_t *)b;
    }
    // One extra page so the start can be page-aligned
    uint8_t *m = mmap(NULL, REGION_BYTES + PAGE, PROT_READ | PROT_WRITE, MAP_PRIVATE | MAP_ANONYMOUS | MAP_NORESERVE, -1, 0);
    if (m == MAP_FAILED) {
        return NULL;
    }
    uintptr_t aligned = ((uintptr_t)m + PAGE - 1) & ~(uintptr_t)(PAGE - 1);
    uintptr_t expected = 0;
    if (!atomic_compare_exchange_strong_explicit(&region_base, &expected, aligned, memory_order_acq_rel, memory_order_acquire)) {
        munmap(m, REGION_BYTES + PAGE);
        return (uint8_t *)expected;
    }
    return (uint8_t *)aligned;
}

static int in_region(const void *p) {
    uintptr_t b = atomic_load_explicit(&region_base, memory_order_acquire);
    return b != 0 && (uintptr_t)p >= b && (uintptr_t)p < b + REGION_BYTES;
}

// A head after one change: the version goes up, the link is `link`
static uint64_t next_head(uint64_t head, uint32_t link) {
    return (((head >> LINK_BITS) + 1) << LINK_BITS) | link;
}

// ---- Runs of pages ----

// `bytes` bytes aligned to `align` (a power of two, at least a page) straight from the OS
static void *os_map(size_t bytes, size_t align) {
    uint8_t *m = mmap(NULL, bytes + align, PROT_READ | PROT_WRITE, MAP_PRIVATE | MAP_ANONYMOUS, -1, 0);
    if (m == MAP_FAILED) {
        return NULL;
    }
    uint8_t *p = (uint8_t *)(((uintptr_t)m + align - 1) & ~(uintptr_t)(align - 1));
    if (p > m) {
        munmap(m, (size_t)(p - m));
    }
    size_t tail = (size_t)(m + bytes + align - (p + bytes));
    if (tail > 0) {
        munmap(p + bytes, tail);
    }
    return p;
}

void *r8a_pages(size_t n) {
    if (n == 0) {
        n = 1;
    }
    if (n > MAX_RUN) {
        return os_map(n << PAGE_SHIFT, PAGE);
    }
    uint8_t *base = region();
    if (base == NULL) {
        return os_map(n << PAGE_SHIFT, PAGE);
    }
    list_t *list = &page_free[n];
    uint64_t head = atomic_load_explicit(list, memory_order_acquire);
    for (;;) {
        uint32_t link = (uint32_t)(head & LINK_MASK);
        if (link == 0) {
            break;
        }
        uint32_t next = atomic_load_explicit(&page_next[link - 1], memory_order_relaxed);
        if (atomic_compare_exchange_weak_explicit(list, &head, next_head(head, next), memory_order_acquire, memory_order_acquire)) {
            return base + ((size_t)(link - 1) << PAGE_SHIFT);
        }
    }
    uint64_t at = atomic_fetch_add_explicit(&page_top, n, memory_order_relaxed);
    if (at + n > REGION_PAGES) {
        // The range is used up: further runs come from the OS
        return os_map(n << PAGE_SHIFT, PAGE);
    }
    return base + ((size_t)at << PAGE_SHIFT);
}

void r8a_free_pages(void *p, size_t n) {
    if (n == 0) {
        n = 1;
    }
    if (!in_region(p)) {
        munmap(p, n << PAGE_SHIFT);
        return;
    }
    // Give the memory back, keep the addresses
    madvise(p, n << PAGE_SHIFT, MADV_DONTNEED);
    uint32_t page = (uint32_t)(((uint8_t *)p - region()) >> PAGE_SHIFT);
    list_t *list = &page_free[n];
    uint64_t head = atomic_load_explicit(list, memory_order_acquire);
    for (;;) {
        atomic_store_explicit(&page_next[page], (uint32_t)(head & LINK_MASK), memory_order_relaxed);
        if (atomic_compare_exchange_weak_explicit(list, &head, next_head(head, page + 1), memory_order_release, memory_order_acquire)) {
            return;
        }
    }
}

// ---- Small blocks ----

// The class for `size` bytes aligned to `align`, or -1 when it needs whole pages
static int class_of(size_t size, size_t align) {
    if (align > MIN_ALIGN) {
        // Only power-of-two classes are aligned beyond 16 bytes: round up to one
        size_t want = size > align ? size : align;
        size_t pow = MIN_ALIGN;
        while (pow < want) {
            pow <<= 1;
        }
        size = pow;
    }
    int lo = 0;
    int hi = CLASSES;
    while (lo < hi) {
        int mid = (lo + hi) / 2;
        if (CLASS_SIZE[mid] < size) {
            lo = mid + 1;
        } else {
            hi = mid;
        }
    }
    return lo < CLASSES ? lo : -1;
}

static uint32_t unit_of(uint8_t *base, void *p) {
    return (uint32_t)(((uint8_t *)p - base) >> UNIT_SHIFT) + 1;
}

static void *block_at(uint8_t *base, uint32_t link) {
    return base + ((size_t)(link - 1) << UNIT_SHIFT);
}

// The link word inside a free block
static _Atomic uint32_t *link_in(void *block) {
    return (_Atomic uint32_t *)block;
}

// Splices the chain first ..= last (already linked first → … → last) onto `list`
static void push_chain(uint8_t *base, list_t *list, void *first, void *last) {
    uint32_t link = unit_of(base, first);
    uint64_t head = atomic_load_explicit(list, memory_order_acquire);
    for (;;) {
        atomic_store_explicit(link_in(last), (uint32_t)(head & LINK_MASK), memory_order_relaxed);
        if (atomic_compare_exchange_weak_explicit(list, &head, next_head(head, link), memory_order_release, memory_order_acquire)) {
            return;
        }
    }
}

static void *pop_block(uint8_t *base, list_t *list) {
    uint64_t head = atomic_load_explicit(list, memory_order_acquire);
    for (;;) {
        uint32_t link = (uint32_t)(head & LINK_MASK);
        if (link == 0) {
            return NULL;
        }
        void *b = block_at(base, link);
        // The block may have been taken and reused meanwhile; then the version moved on and the CAS fails
        uint32_t next = atomic_load_explicit(link_in(b), memory_order_relaxed);
        if (atomic_compare_exchange_weak_explicit(list, &head, next_head(head, next), memory_order_acquire, memory_order_acquire)) {
            return b;
        }
    }
}

// An empty class takes a page: the first block is returned, the rest go onto the list in one splice
static void *refill(uint8_t *base, int c) {
    uint8_t *page = r8a_pages(1);
    if (page == NULL || !in_region(page)) {
        if (page != NULL) {
            r8a_free_pages(page, 1);
        }
        return NULL;
    }
    size_t size = CLASS_SIZE[c];
    size_t count = PAGE / size;
    if (count > 1) {
        for (size_t k = 1; k + 1 < count; k++) {
            atomic_store_explicit(link_in(page + k * size), unit_of(base, page + (k + 1) * size), memory_order_relaxed);
        }
        push_chain(base, &block_free[c], page + size, page + (count - 1) * size);
    }
    return page;
}

void *r8a_alloc(size_t size, size_t align) {
    int c = class_of(size, align);
    if (c < 0) {
        size_t pages = (size + PAGE - 1) >> PAGE_SHIFT;
        return align > PAGE ? os_map(pages << PAGE_SHIFT, align) : r8a_pages(pages);
    }
    uint8_t *base = region();
    if (base == NULL) {
        return NULL;
    }
    void *b = pop_block(base, &block_free[c]);
    return b != NULL ? b : refill(base, c);
}

void r8a_free(void *p, size_t size, size_t align) {
    if (p == NULL) {
        return;
    }
    int c = class_of(size, align);
    if (c < 0) {
        size_t pages = (size + PAGE - 1) >> PAGE_SHIFT;
        if (align > PAGE || !in_region(p)) {
            munmap(p, pages << PAGE_SHIFT);
        } else {
            r8a_free_pages(p, pages);
        }
        return;
    }
    push_chain(region(), &block_free[c], p, p);
}
