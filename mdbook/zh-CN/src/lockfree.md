# 无锁基础件

`crates/j8lockfree`：j8 手写的无锁基础件（`no_std` + `alloc`），`j8framework` 与 `j8taskqueue` 共用。j8 的规矩是
**堆与调度全程无锁**，无锁结构自己写，⛔ 引入 crossbeam。

| 类型 | 是什么 |
|---|---|
| `Fifo` | 多生产者多消费者先进先出队列（位置单调递增、分段槽表） |
| `SegVec` | 只追加的分段向量：下标稳定、可并发读；整理与取出要独占 |
| `PushList` | 只压入的 Treiber 栈：整表取走或封口（封口后压入被退回） |
| `Stock` | 库存：独占时整理成只读数组，并发时原子游标成批领取 |
| `Deque` | Chase–Lev 工作窃取双端队列：主人凭据 `Owner` 压 / 弹，别人偷 |
| `OnceBox` | 只发布一次的惰性值（并发初始化各算各的，赢的留下） |
| `AtomicRefCell` | 可跨线程的 `RefCell`：借用冲突时报错，从不等待 |

## 共同的做法：只增不减，独占时释放

并发期间**只增加内存**（新段、旧缓冲留着），释放推迟到独占（拿到 `&mut` 或析构）时。所以：

- 不需要危险指针（hazard pointer）；
- 不需要纪元回收（epoch-based reclamation）；
- 没有 ABA 问题。

代价是并发期间内存只涨不落，但这些结构的使用方式（事件循环的队列、任务图的下游表）都有天然的独占时刻可以整理。

## 谁在用

| 使用方 | 用什么 |
|---|---|
| j8taskqueue | `Fifo` 作就绪队列、`PushList` 作下游表 |
| j8framework 事件循环 | 消息队列、工作窃取 |
| j8binding 宿主对象 | `AtomicRefCell` 包着 `Box<dyn Any + Send + Sync>` |
| j8executor | 内联缓存与加载表 |
