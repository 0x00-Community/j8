# 堆与 GC：建在 paragc 上

j8 的堆是 [paragc](https://crates.io/crates/paragc)，j8 **只用它的公开接口**（`ScopeLocal` / `Slot` / `Handle` / `Scope` /
`ScopeGlobal` / `Weak`），不碰它的内部。`j8framework/src/gc.rs` 在上面补 JS 引擎需要的东西。

## paragc 是什么

一个独立的 Rust GC 库（同属 0x00 社区）：

- Immix 式的块 / 行堆；
- 年轻回收 + 老对象引用计数（LXR 式），稀疏块疏散；
- 标记与清扫并行，内部无锁；
- 堆有自己的回收线程（数目同平台线程池）；
- 公开堆、跨进程堆等扩展能力（[jumpheap](workerd.md#jumpheap) 用到）。

## 生命周期

paragc 的类型带两个生命周期：堆的 Brand `'heap` 和 scope 的 `'scope`。j8 内部只有一个：`'scope`，即一次 `enter`
的 Brand。其余的由 isolate 藏起来：

- `Isolate<'p>` 拥有一个 `Heap<'p>`（用 `Heap::new_unchecked` 创建，才能放进结构体）；每次 `enter` 开一个独占的
  paragc scope，并换成这次进入的 `'scope`，所以一次进入的东西留不到下一次；
- j8 的别名把 paragc 的两个生命周期并成一个：`ScopeLocal<'scope, T>`、`Slot<'scope, T>`、`Handle<'scope, T>`；
- 要活过一次进入的东西是根 `ScopeGlobal<'p, B>`（Brand `B` 标明类型：`ValueBrand`、`ContextBrand` …），由
  `mc.stash` 建、`mc.fetch` 读回，或者是 `Weak<'p, B>`；
- 执行器对载入的代码持有弱引用，所以是 `Executor<'p>`。

## 待处理异常

照 V8 的做法，抛出的值不放在 `Error` 里：`Error::Exception` 只表示"JS 抛了"，值在这次进入的待处理槽里
（`mc.raise` / `scope.take_exception`）。进入结束时还留在槽里的值会被挂到根上，下次进入开始时重新抛出，宿主因此能读到它
（`Runtime::exception_text`）。宿主代码要跨轮持有一个 reject 原因时，把它暂存进 isolate（`scope.park()` ⇒
`Error::Parked(ticket)`，`scope.unpark(ticket)`）。

## j8 的 GC 层

paragc 里对象中能改的边只有 `Slot`（带写屏障），所以 j8 的可变堆数据分两种放：

| 单元 | 放什么 | 怎么改 |
|---|---|---|
| `GcCell` | 一整块可变数据（`ObjectState` 等） | 当前版本放在槽指着的**快照**里：快照是本轮（上次回收后）新分配的就原地改；否则先挪进一份新快照再改，槽换过去时写屏障就记下了它。每块数据每个回收周期最多挪一次 |
| `ValueCell` | 一个 `Value`（环境槽、属性槽） | 字 + 槽 |
| `Cow`、`GcQueue`、`Static` | 写时复制、堆上队列、跳过追踪的数据 | — |

堆类型用 `#[derive(Collect)]`（在 j8macros，生成 paragc 的 `Trace`）；`#[collect(require_static)]` 跳过一个字段
（放在类型上表示整个类型没有 GC 边）。有边的类型不许实现 `Drop`。

## 单线程的 isolate

一个 isolate 同一时刻只在一个线程上跑 JS（V8 的模型），它堆里的对象只由这个线程读写，所以借用标志、标量字段都是
普通读写（`IsoCell`），⛔ 原子读改写。只有 paragc 的 `Slot` 自带原子（写屏障需要）。

堆里的类型都要 `Send + Sync`（paragc 的规矩）：宿主数据是 `Box<dyn HostObject + Send + Sync>`，rope 用 `Arc`。

## 规矩

这些规矩写在代码注释里，违反会造成悬垂或漏标：

- **回收只在 `enter` 之外**、所有 `ScopeLocal` 都挂在根上时发生，而且**对象会被搬**。⛔ 把 `ScopeLocal` 或对象地址存进
  `Trace` 不遍历的地方（`Static`、旁路缓存）。按地址哈希的东西（Map / Set 的索引）在 `relocate` 里更新（见
  `Collection`）。这也是内联缓存按 shape 的 Handle（`ShapeKey`，即 Handle 的编号，shape 被搬时它不变）而不按地址识别的原因。
- ⛔ **给还没进堆的值写槽**：写屏障记的是槽的地址。先分配进堆再填（见 `Scope::context`）；`#[heap(new)]` 生成的
  `new` 在进堆之前填单元，不经屏障。
- **只做停顿式回收**（`collect_if_needed` / `collect`，内部并行）：`GcCell` 原地改年轻快照不经写屏障，
  所以 ⛔ `mark_concurrently`。
- 年轻回收用 paragc 缺省的 `YoungEvacuation::SkipRemembered`：老对象直接指着的年轻对象不动，被搬的对象字段都走
  `relocate`，`ValueCell` 字里的地址随之改写。⛔ `YoungEvacuation::Full`：它会改老对象槽里的地址，但不知道
  `ValueCell` 字里另存的那一份。

## 什么时候回收

- `Isolate::enter` 返回后，分配够多就回收（通常年轻回收，每隔几轮一次完整回收）；
- 执行器分片执行：每片最多一万条指令，停下时把执行状态挂到根上，`enter` 返回时回收一段再接着跑；
- 宿主函数里重入的 JS 在当前片里跑完，期间不回收。

## 根、边与环

`ScopeGlobal` 是根：drop 之前一直保活它的对象，所以只属于堆外的代码（宿主、线程、事件循环里待办的工作）。宿主对象在堆里：
它持有的 JS 值是被追踪的字段，经由它形成的环（回调捕获了持有它的对象）和别的垃圾一样被回收。`WeakRef` /
`FinalizationRegistry` 目前按强引用实现。
