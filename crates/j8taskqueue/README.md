# j8taskqueue

**English** · [简体中文](README.zh-CN.md)

A lock-free dataflow task queue for [j8](https://github.com/0x00-Community/j8) (`no_std` + `alloc`). Tasks run in
parallel on a [moremath](https://crates.io/crates/moremath) `Executor` (the host passes a `ThreadMulti`; `ThreadSingle`
works without std).

- `ITask`: a task — `recursable` (may it add tasks / ask to run again), `depend` (what must finish first), `exec` (run
  it with a bytecode `Quota`)
- `ITaskPoll` / `TaskQueue`: `add` a task to get its `TaskId`, `run` the event loop
- `TaskId::family` finds the family root (union-find), `TaskId::cancel` cancels a whole family

Design: dataflow with no round barrier (a task whose dependencies are done is ready at once, on any idle thread); fully
lock-free (atomic dependency counts, sealable `PushList` successor lists, the segmented `Fifo` ready queue); no cycles
(dependencies can only name tasks already added); quotas count bytecodes, and a task that runs out yields and queues
again; tasks borrow for `'a` instead of being `'static`.

## License

MIT, as part of [j8](https://github.com/0x00-Community/j8).
