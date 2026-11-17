# j8lockfree

**English** · [简体中文](README.zh-CN.md)

Hand-written lock-free building blocks for [j8](https://github.com/0x00-Community/j8) (`no_std` + `alloc`), shared
by `j8framework` and `j8taskqueue`.

| Type | What it is |
| --- | --- |
| `Fifo` | Multi-producer multi-consumer FIFO queue (monotonic positions, segmented slot table) |
| `SegVec` | Append-only segmented vector: stable indices, concurrent reads; tidying and taking out need exclusive access |
| `PushList` | Push-only Treiber stack: take the whole list or seal it (pushes after sealing are handed back) |
| `Stock` | A stock tidied into a read-only array when exclusive, claimed in batches by an atomic cursor when shared |
| `Deque` | Chase–Lev work-stealing deque: the owner (`Owner`) pushes and pops, others steal |
| `OnceBox` | A lazily computed value published once (racing initializers each compute, the winner stays) |
| `AtomicRefCell` | A `RefCell` usable across threads: a borrow conflict is an error, never a wait |

The common rule: while shared, memory only grows (segments, old buffers); freeing waits for exclusive access (`&mut` or
drop), so there are no hazard pointers, no epochs and no ABA.

## License

MIT, as part of [j8](https://github.com/0x00-Community/j8).
