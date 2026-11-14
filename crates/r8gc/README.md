# r8gc

A generational, moving (compacting), parallel GC for Rust with a `'gc` brand lifetime; `no_std` (requires `alloc`).

- The young generation uses per-block bump allocation; a minor collection starts from the roots and the remembered set
  and copies live objects into the old generation in parallel (the write barrier maintains the remembered set)
- The old generation is paged by size class; a major collection marks incrementally (epoch flip, backward write
  barrier), and marking / sweeping / compaction all run in parallel on the thread pool
- The thread pool is a borrowed `Pool`: [moremath](https://crates.io/crates/moremath)'s `ThreadMulti` / `ThreadSingle`
  work directly, and several `R8Heap`s can share one
- Collection happens only outside `R8Heap::mutate`, when every `Gc` is reachable from the roots, so objects can move:
  besides `trace`, `Collect` has `relocate`

The API follows the shape of [gc-arena](https://github.com/kyren/gc-arena) (`Gc` / `Lock` / `RefLock` / `Rootable!` /
`DynamicRootSet`).

## Usage

```rust
use moremath::ThreadMulti;
use r8gc::{Collect, Gc, R8Heap, RefLock, Rootable};

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
let mut heap: R8Heap<'_, Rootable![Root<'_>]> = R8Heap::new(&pool, |_| Root { head: None });
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
actually dropped in parallel with `R8Heap::set_parallel_drop(true)`. With glibc's malloc, freeing across threads
contends for the original thread's arena lock, so parallel is actually slower; turn it on with an allocator like
jemalloc / mimalloc.

## Features

- `std` (on by default): yield the CPU while waiting. For `no_std` use `default-features = false` (it can only spin,
  which slows parallel collection on a busy machine)
- `verify`: verify every edge in the old generation after each collection (for debugging; very slow)

## Tests

Run `tests/gc.rs` one test at a time (`cargo test --test gc -- --exact <name>`); add `--features verify` to include
heap verification.

## License

MIT. Parts of the design and code come from gc-arena; see `LICENSE-gc-arena` for its license.
