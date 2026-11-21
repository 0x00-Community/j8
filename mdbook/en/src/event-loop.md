# Event loops and the task queue

j8 has no async runtime (no tokio). Its event loop is its own: `j8framework::EventLoop` is built on a `j8taskqueue`
queue, and an `EventLoopSet` pins thousands of event loops onto a few loop threads.

## One event loop

An `EventLoop` is one JS event loop: its own isolate, engine and context, and a j8taskqueue queue. **The queue is the
event loop.** It holds three kinds of tasks:

1. **scripts**, which run a check interval of steps at a time (about a thousand instructions by default; V8's interrupt
   check at frame entries and back edges) and then yield back to the queue;
2. **settlements**, which resolve a promise with what host work produced and drain the microtasks;
3. **polls** of host futures that were woken (no JS runs in them; a ready future queues its settlement).

Scripts and settlements are chained: each waits for the one before, so JS runs one task at a time, to completion. Polls
wait for nothing. So every time the bytecode advances, the queue is checked: host work that became ready meanwhile, from
any thread, is polled before the script goes on, and its JS runs right after the script.

When nothing can run, the thread **parks** until a waker fires. `run_until_idle` returns once no script and no future
of the event loop is outstanding.

## Many event loops: EventLoopSet

```rust,ignore
j8::EventLoopSet::scope(&platform, NonZeroUsize::new(8).unwrap(), |set| {
    // 8 loop threads for the length of the closure
    let rt = j8::Runtime::on::<MyHost>(set, Executor::default())?;
    …
});
```

- **Pinned, no claiming.** An event loop lives on one thread at a time and isn't shared, so a thread runs its event loops
  with no locks: 8 threads and 1000 event loops make 125 each. A thread keeps a local queue of its event loops that have
  work and gives each a turn in turn.
- **Messages only.** Other threads reach a loop thread by a **wake** (an event loop has something new; deduplicated by a
  flag) or a **message** (create an event loop, work in its context, move one in or out), and wait for the answer. A
  message for an event loop that has moved is forwarded.
- **Load balancing.** New event loops go to the least loaded thread. A thread with nothing to run asks the busiest one
  for half the difference, which hands over the event loops waiting in its queue first (work stealing).
  `set.rebalance()` evens the threads out on demand, `set.split(thread)` starts a new thread and gives it half of
  another's. Event loops move only between turns, with their isolate, engine, paused script and pending futures.
- **One quota.** Every turn runs the set's slice of steps (`set.set_slice`) times the event loop's weight
  (`EventLoop::set_weight`), checking the queue every `set.set_check` steps; `set.loads()`, `set.spent()` and
  `EventLoop::spent()` show where the work went. A long script doesn't hold up the other event loops on its thread.

How hosts write async bindings on it: [Async and promises](async.md).

## j8taskqueue

`crates/j8taskqueue`: a lock-free dataflow task queue (`no_std` + `alloc`, depending on nothing but j8lockfree).

| Name | Role |
|---|---|
| `ITask` | a task: `recursable` (may it add tasks or ask to run again), `depend` (what must finish first), `exec` (run with a bytecode quota) |
| `ITaskPoll` / `TaskQueue` | the queue: `add` a task for a `TaskId`, `run` to start |
| `Queue` | the queue without threads, kept by the host (one per event loop): `run` on the calling thread, `run_on` spread over a pool |
| `Pool` | a thread pool (some threads; runs a batch of closures and returns when all are done), implemented by the host; j8framework's `Pool` is this one |
| `TaskId` | a task handle; `family` finds the family's root (union-find), `cancel` cancels the whole family |
| `Quota` | a bytecode quota: the executor `spend`s one per instruction |

The design:

- **dataflow, no barriers**: when a task finishes, the tasks depending on it that have everything go straight to the
  ready queue, taken by threads still in their loop. a and b both needed by c, independent of each other ⇒ a and b run in
  parallel, and c runs the moment both are done, waiting for nothing unrelated;
- **no spinning**: a thread that finds no work leaves; work that becomes ready later is run by the thread that made it ready;
- **lock-free throughout**: dependency counts, downstream lists (a sealable `PushList`), union-find parents and the
  ready queue (a segmented MPMC `Fifo`) are all atomics;
- **no cycles**: dependencies can only name tasks already added, read once when added ⇒ no deadlock;
- **quotas count bytecodes**: a task out of quota returns `Flow::Yield` and queues again; each `run` also has a total
  budget, after which it returns `Stop::Budget`;
- **borrowing, not `'static`**: tasks live in `'a` and may borrow the caller's data; only `Send` is required.
