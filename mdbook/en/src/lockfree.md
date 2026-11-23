# Lock-free building blocks

`crates/j8lockfree`: j8's hand-written lock-free building blocks (`no_std` + `alloc`), shared by `j8framework` and
`j8taskqueue`. j8's rule is that **the heap and scheduling are lock-free throughout**, with the structures written by
hand, ⛔ crossbeam.

| Type | What it is |
|---|---|
| `Fifo` | multi-producer multi-consumer FIFO queue (monotonic positions, a segmented slot table) |
| `SegVec` | append-only segmented vector: stable indices, concurrent reads; compaction and removal need exclusive access |
| `PushList` | push-only Treiber stack: take the whole list, or seal it (pushes after sealing are handed back) |
| `Stock` | a stock: tidied into a read-only array when exclusive, claimed in batches by an atomic cursor when concurrent |
| `Deque` | Chase–Lev work-stealing deque: the owner (`Owner`) pushes and pops, others steal |
| `OnceBox` | a lazily published value (concurrent initializers each compute, the winner stays) |
| `AtomicRefCell` | a `RefCell` usable across threads: a borrow conflict is an error, never a wait |

## The common trick: only grow, free when exclusive

While concurrent, the structures **only add memory** (new segments; old buffers stay), and freeing waits for exclusive
access (`&mut` or drop). So there is:

- no need for hazard pointers;
- no need for epoch-based reclamation;
- no ABA problem.

The cost is memory that only grows while concurrent, but the way these structures are used (event loop queues, the
downstream lists of a task graph) always has natural exclusive moments to tidy up in.

## Users

| User | Uses |
|---|---|
| j8taskqueue | `Fifo` as the ready queue, `PushList` as downstream lists |
| j8executor event loops | message queues, work stealing |
| j8binding host objects | a `GcCell` around `Box<dyn HostObject + Send + Sync>` |
| j8executor | inline caches and load tables |
