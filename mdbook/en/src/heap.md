# Heap and GC: built on paragc

j8's heap is [paragc](https://crates.io/crates/paragc), and j8 **uses only its public interface** (`ScopeLocal` /
`Slot` / `Handle` / `Scope` / `ScopeGlobal` / `Weak`), never its internals. `j8framework/src/gc.rs` adds what a JS
engine needs on top.

## paragc

A standalone Rust GC library from the same community:

- an Immix-style heap of blocks and lines;
- young collection plus reference counting of old objects (LXR-style), evacuation of sparse blocks;
- parallel marking and sweeping, lock-free inside;
- collector threads of its own (as many as the platform's pool);
- public heaps and cross-process heaps ([jumpheap](workerd.md#jumpheap) uses them).

## Lifetimes

paragc types carry two lifetimes, the heap's brand `'heap` and the scope's `'scope`. Inside j8 there is only one:
`'scope`, the brand of one `enter`. The isolate hides the rest:

- `Isolate<'p>` owns a `Heap<'p>` (made with `Heap::new_unchecked`, so it can live in a struct); every `enter` opens one
  exclusive paragc scope and rebrands it to the entry's `'scope`, so nothing of one entry can be kept for the next;
- j8's aliases fold both paragc lifetimes into one: `ScopeLocal<'scope, T>`, `Slot<'scope, T>`, `Handle<'scope, T>`;
- what must survive an entry is a root, `ScopeGlobal<'p, B>` (the brand `B` names its type: `ValueBrand`,
  `ContextBrand`, …), made by `mc.stash` and read back by `mc.fetch`, or a `Weak<'p, B>`;
- the executor, which keeps weak references to its loaded code, is `Executor<'p>`.

## Pending exception

As in V8, a thrown value doesn't travel inside `Error`: `Error::Exception` only says "JS threw", and the value waits in
the entry's pending slot (`mc.raise` / `agent.take_exception`). A value left pending when an entry ends is rooted and
raised again at the start of the next one, so the host can read it (`Runtime::exception_text`). Host code that must hold
a rejection across turns parks it in the isolate (`agent.park()` ⇒ `Error::Parked(ticket)`, `agent.unpark(ticket)`).

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

Heap types must be `Send + Sync` (paragc's rule): host data is a `Box<dyn HostObject + Send + Sync>`, ropes use `Arc`.

## Rules

These are written in the code; breaking them dangles pointers or loses marks:

- **Collection happens only outside `enter`**, when every `ScopeLocal` hangs off a root, and **objects move**. ⛔
  storing a `ScopeLocal` or an object address where `Trace` doesn't look (`Static`, side caches). Things hashed by
  address (the indexes of Map / Set) are updated in `relocate` (see `Collection`). This is also why inline caches key
  on shape handles (`ShapeKey`, the handle's id, which stays put while the shape moves), not addresses.
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

## Roots, edges and cycles

A `ScopeGlobal` is a root: it keeps its object alive until it is dropped, so it belongs only to code outside the heap
(the host, a thread, an event loop's pending work). A host object is in the heap: its JS values are traced fields, so a
cycle through it (a callback that captures the object holding it) is collected like any other. `WeakRef` /
`FinalizationRegistry` hold strong references for now.
