# j8taskqueue

[English](README.md) · **简体中文**

[j8](https://github.com/0x00-Community/j8) 的无锁数据流任务队列（`no_std` + `alloc`）。任务在 [moremath](https://crates.io/crates/moremath)
的 `Executor` 上并行跑（宿主给 `ThreadMulti`；没有 std 时可用 `ThreadSingle`）。

- `ITask`：一个任务——`recursable`（能不能塞新任务、能不能要求再跑）、`depend`（要等谁先做完）、`exec`（带字节码配额 `Quota` 执行）
- `ITaskPoll` / `TaskQueue`：`add` 塞一个任务拿到 `TaskId`，`run` 启动事件循环
- `TaskId::family` 找家族的根（并查集），`TaskId::cancel` 取消整个家族

设计：数据流、没有轮次屏障（依赖做完的任务立刻就绪，任意空闲线程都能拿走）；全程无锁（原子依赖计数、可封口的 `PushList` 下游表、分段 `Fifo`
就绪队列）；图不成环（依赖只能写已经塞进来的任务）；配额按字节码条数，用完的任务让出、回队列排队；任务借用 `'a` 而不要求 `'static`。

## 许可

MIT，随 [j8](https://github.com/0x00-Community/j8) 一起。
