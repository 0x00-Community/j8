# j8gc

A generational, moving (compacting), parallel GC for Rust with a `'gc` brand lifetime; `no_std` (requires `alloc`).

- The young generation uses per-block bump allocation; a minor collection starts from the roots and the remembered set
  and copies live objects into the old generation in parallel (the write barrier maintains the remembered set)
- The old generation is paged by size class; a major collection marks incrementally (epoch flip, backward write
  barrier), and marking / sweeping / compaction all run in parallel on the thread pool
- The thread pool is a borrowed `Pool`: [moremath](https://crates.io/crates/moremath)'s `ThreadMulti` / `ThreadSingle`
  work directly, and several `J8Heap`s can share one
- Collection happens only outside `J8Heap::mutate`, when every `Gc` is reachable from the roots, so objects can move:
  besides `trace`, `Collect` has `relocate`
- Lock-free throughout (no locks, not even spin locks): any number of threads may `mutate` one heap at once, and
  collection waits at a safepoint gate until none is inside (see "Threads" below)

The API follows the shape of [gc-arena](https://github.com/kyren/gc-arena) (`Gc` / `Lock` / `RefLock` / `Rootable!` /
`DynamicRootSet`).

## Usage

```rust
use moremath::ThreadMulti;
use j8gc::{Collect, Gc, J8Heap, RefLock, Rootable};

#[derive(Collect)]
#[collect(no_drop)]
struct Node<'gc> {
    value: u64,
    next: Option<Gc<'gc, RefLock<Node<'gc>>>>,
}

#[derive(Collect)]
#[collect(no_drop)]
struct Root<'gc> {
    head: Option<Gc<'gc, RefLock<Node<'gc>>>>,
}

let pool = ThreadMulti::from(std::num::NonZeroUsize::new(4).unwrap());
let mut heap: J8Heap<'_, Rootable![Root<'_>]> = J8Heap::new(&pool, |_| Root { head: None });
heap.mutate_root(|mc, root| {
    for value in 0..1000 {
        root.head = Some(Gc::new(mc, RefLock::new(Node { value, next: root.head })));
    }
});
heap.collect_all();
```

For collection time versus thread count, see `cargo run --release --example bench -- <threads>`.

## Parallelism

Each collection phase is split into tasks handed to the borrowed `Pool` (with built-in work stealing): marking, copying
promotion in minor collections, sweeping, and moving and pointer updating during compaction all run in parallel.
When there is little work it is done on the calling thread first (without involving the thread pool); while waiting,
threads spin first and yield the CPU after a while (`std` feature).

Dead objects are dropped on the calling thread by default: a type's `Collect::PARALLEL_DROP` (inferred from the fields
by the derive macro: `no_drop` and every field allows it) says it can be dropped on another thread, but objects are only
actually dropped in parallel with `J8Heap::set_parallel_drop(true)`. With glibc's malloc, freeing across threads
contends for the original thread's arena lock, so parallel is actually slower; turn it on with an allocator like
jemalloc / mimalloc.

## Threads

- The heap is plain memory, not tied to any thread: its state changes and growth are atomic and lock-free (the
  lock-free building blocks are hand-written in the repository's `j8lockfree` crate)
- `J8Heap` is `Sync` when its root is; a `Gc` is `Send` / `Sync` when its target is `Sync`. `Lock<T>` is an atomic
  one-word cell (`T: Word`; `Lock<Option<Gc<_>>>` is an atomic pointer slot with `compare_exchange`), so it may be
  shared; `RefLock<T>` is a single-threaded `RefCell` and keeps a heap single-threaded
- Each mutating thread gets its own `Mutation`: it bump-allocates in its own buffer and records write-barrier hits
  locally, handing them to the collector when it leaves `mutate`. `mutate_in(&mut tlab, …)` keeps a thread's
  allocation buffer (`Tlab`) across calls; plain `mutate` borrows the heap's own buffer when it is free
- Collection: `collect_debt` / `collect_all` through `&mut self` (exclusive by borrowing), or `collect_debt_shared` /
  `collect_all_shared` through `&self`: the gate (an atomic word: a request bit plus the number of threads inside
  `mutate`) makes new mutators wait and waits for the current ones to leave. Never call the shared ones from inside
  `mutate`

## Features

- `std` (on by default): yield the CPU while waiting. For `no_std` use `default-features = false` (it can only spin,
  which slows parallel collection on a busy machine)
- `verify`: verify every edge in the old generation after each collection (for debugging; very slow)

## Tests

Run `tests/gc.rs` and `tests/threads.rs` one test at a time (`cargo test --test gc -- --exact <name>`); add
`--features verify` to include heap verification. `tests/threads.rs` (several mutators while others collect) also runs
under Miri at a reduced size.

## License

MIT. Parts of the design and code come from gc-arena; see `LICENSE-gc-arena` for its license.
