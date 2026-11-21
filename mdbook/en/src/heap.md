# Heap and GC: built on paragc

j8's heap is [paragc](https://crates.io/crates/paragc), and j8 **uses only its public interface** (`Gc` / `Slot` /
`Mutator` / `Root`), never its internals. `j8framework/src/gc.rs` adds what a JS engine needs on top.

## paragc

A standalone Rust GC library from the same community:

- an Immix-style heap of blocks and lines;
- young collection plus reference counting of old objects (LXR-style), evacuation of sparse blocks;
- parallel marking and sweeping, lock-free inside;
- collector threads of its own (as many as the platform's pool);
- public heaps and cross-process heaps ([jumpheap](workerd.md#jumpheap) uses them).

## j8's GC layer

In paragc the only mutable edge inside an object is a `Slot` (with a write barrier), so j8 keeps its mutable heap data in
two kinds of cell:

| Cell | Holds | How it changes |
|---|---|---|
| `GcCell` | a block of mutable data (`ObjectState` …) | the current version lives in a **snapshot** a slot points to: a snapshot allocated this cycle (since the last collection) is changed in place; an older one is first moved into a new snapshot, and swapping the slot records it in the write barrier. Each block moves at most once per cycle |
| `ValueCell` | one `Value` (environment and property slots) | a word plus a slot |
| `Cow`, `GcQueue`, `Static` | copy-on-write, a queue on the heap, untraced data | — |

Heap types use `#[derive(Collect)]` (in j8macros, generating paragc's `Trace`); `#[collect(require_static)]` skips a
field (on a type: the whole type has no GC edges). Types with edges may not implement `Drop`.

## Single-threaded isolates

An isolate runs JS on one thread at a time (V8's model), and only that thread reads and writes the objects in its heap,
so borrow flags and scalar fields are plain loads and stores (`IsoCell`), ⛔ atomic read-modify-write. Only paragc's
`Slot` is atomic (the write barrier needs it).

Heap types must be `Send + Sync` (paragc's rule): host data is a `Box<dyn Any + Send + Sync>`, shapes and ropes use `Arc`.

## Rules

These are written in the code; breaking them dangles pointers or loses marks:

- **Collection happens only outside `enter`**, when every `Gc` hangs off a root, and **objects move**. ⛔ storing a `Gc`
  or an object address where `Trace` doesn't look (`Static`, side caches). Things hashed by address (the indexes of Map
  / Set) are updated in `relocate` (see `Collection`). This is also why inline caches key on shapes (`Arc`), not objects.
- ⛔ **writing a slot of a value not yet on the heap**: the barrier records the slot's address. Allocate first, then fill
  (see `Scope::context`); the `new` that `#[heap(new)]` generates fills cells before that, without the barrier.
- **Stop-the-world collection only** (`collect_if_needed` / `collect`, parallel inside): `GcCell` changes young
  snapshots in place without the barrier, so ⛔ `mark_concurrently`.
- Young collection uses paragc's default `YoungEvacuation::SkipRemembered`: young objects old ones point to directly
  stay, fields of moved objects go through `relocate`, and addresses in `ValueCell` words are rewritten. ⛔
  `YoungEvacuation::Full`: it rewrites the addresses in old objects' slots but doesn't know the copy in `ValueCell` words.

## When it collects

- after `Isolate::enter` returns, if enough was allocated (usually young, a full one every few);
- the executor runs in slices: at most ten thousand instructions, then the execution state is rooted and a collection
  can run when `enter` returns, before the run continues;
- JS re-entered from a host function runs to completion inside the current slice, without collection.

## Persistent handles and cycles

`Global`, and `j8::Persistent` / `j8::Function` in bindings, are GC roots. A host object that stores a value pointing
back to itself keeps both alive until it is dropped from Rust. `WeakRef` / `FinalizationRegistry` hold strong references
for now.
