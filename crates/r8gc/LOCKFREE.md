# r8gc 无锁化方案

目标（2026-11-15 定）：堆只是一块内存，和线程池无关；堆的状态变化和增长全部用原子操作、无锁，**禁止任何锁（含自旋锁）**；
允许多个线程同时分配、写对象、触发回收。上层要的是同一个 realm 的 JS 任务也能并行（r8taskqueue 的数据流调度）。
无锁结构全部手写（⛔ crossbeam 及同类库）。

## 现状：哪里不是无锁的

| 位置 | 现在 | 问题 |
|---|---|---|
| `heap.rs` 老年代 | `Spin<OldSpace>`：页表 `Vec<Page>`、各档空闲表 `Vec<Ptr>`、大对象表、`bytes` 都在一把自旋锁下 | 是锁；晋升缓冲 `Lab` 每批槽都要拿它 |
| `parallel.rs` 工作窃取 | 公共栈 `Spin<Vec<Ptr>>` | 是锁 |
| `context.rs` 回收器 | `Cell<Phase>` / `Cell<bool>` 纪元 / 碰撞游标 `Cell<usize>`、`RefCell<Nursery>` / 灰栈 / 记忆集 | 单线程内部可变性，`Context` 不是 `Sync`，只能一个线程分配 |
| `arena.rs` | `Mutation` 就是 `Context` 本身（`repr(transparent)`） | 所有分配共用一个游标，没有线程本地分配区 |
| `roots.rs` 动态根 | `Rc<Slots>`，槽表 `RefCell<Vec<…>>` | 不能跨线程 |
| `metrics.rs` | `Cell<usize>` | 不能跨线程 |
| `lock.rs` | `Lock<T>` = `Cell<T>`，`RefLock<T>` = `RefCell<T>` | 对象内容只能一个线程改；r8framework 里 `JsObject = Gc<RefLock<ObjectData>>`，`borrow_mut(mc)` 有 188 处 |

头（`Header`）已经全是原子位，标记 / 认领 / 转发本来就是无锁并行的，可以保留。

## 分期

每期独立可验证：r8gc 自己的测试（含 `--features verify` 堆校验）、Miri（Stacked + Tree Borrows）、test262 基线零回归。

### P0 无锁基础件（新 crate `r8lockfree`，`no_std`）

r8gc 和 r8taskqueue 共用，各自不再复制：

- `Fifo<T>`：分段 MPMC 先进先出队列（从 r8taskqueue 挪过来）
- `SegVec<T>`：只追加的分段向量：`len.fetch_add` 占位、段 CAS 装上、并发期间不拆；下标稳定，可以并发读
- `PushList<T>`：只压入的 Treiber 栈，整表 `swap` 取走（没有并发弹出，所以没有 ABA）
- `Deque<T>`：Chase–Lev 工作窃取双端队列（本地压 / 弹、别人偷），缓冲区扩容时旧缓冲留到独占时再释放

验证：每个结构都写多线程压力测试，加 Miri。

### P1 回收器内部去锁（对外 API 不变）

- **老年代页表**：`SegVec<Page>`；开新页 = 分配 + `push`。`bytes` 改成 `AtomicUsize`
- **空闲槽**：清扫是在所有 mutator 都停着的时候做的，于是清扫完把各档空闲槽冻结成一个只读数组，配一个原子游标；
  领槽 = `游标.fetch_add(一批)` 取一段下标。数组在两次回收之间不变，所以没有 ABA，也不用回收节点。领完了就开新页
- **大对象**：`PushList`，清扫时独占整理
- **工作窃取**：公共栈换成每个工人一个 `Deque`，空了随机偷别人的；结束判定沿用现在的 `idle == started` 计数
- **Metrics**：`AtomicUsize`

做完这一期，r8gc 里就没有锁了，但还是单 mutator。

### P2 多个 mutator 同时用一个堆

- `R8Heap: Sync`；`mutate(&self)` 可以在多个线程上同时调用
- **`Mutation` 改成每个线程一份**：自己的碰撞区（线程本地分配区，新生代块从原子块表领），
  自己的记忆集 / 灰栈 / 待析构缓冲。`Mutation` 是 `Send`、不是 `Sync`，所以缓冲不需要原子操作
- **写屏障**：标志位用 `fetch_or` 拿旧值，只有亲手把 `REMEMBERED` / `GRAY` 从 0 改成 1 的那个线程把对象记进自己的缓冲，
  不重复也不需要锁。`phase` / `epoch` 改成原子量
- **安全点**：回收会搬对象，所以搬的时候不能有 mutator 拿着裸 `Gc`。用一个原子状态字 = 「请求回收」位 + 「在 mutate 里的线程数」：
  - 进 `mutate`：没有请求就 CAS 把计数加一；有请求就退避等回收做完
  - 出 `mutate`：计数减一，把本线程的缓冲交到全局的 `PushList`
  - 回收方：置请求位，等计数归零，做回收，清位
  - r8 的 `Engine::eval` 本来每一万条指令就出一次 `enter`，天然就是安全点
- **动态根**：`Arc` + `SegVec` 槽表 + 空槽 `PushList`
- 验证：多线程同时分配、写屏障、触发回收的压力测试（开 `verify`），加 Miri 小规模版本

### P3 对象内容多线程可写

- `Lock<T>`：限 `T: Copy` 且不超过 8 字节，内部用 `AtomicU64`，`get` / `set` 是原子读写
- `RefLock<T>`：`RefCell` 的独占借用本身就是锁的语义，只能拆掉。改成**字段级原子**：
  对象的可变部分拆成原子槽，每个槽可以独立地原子读写、CAS
- **`AtomicValue`**：`Value` 现在 16 字节（标签 + 载荷），riscv64 上没有 16 字节 CAS。
  所以存储槽改用 8 字节 NaN-boxing 的 `AtomicValue(AtomicU64)`，读出来时 `From<u64>` 还原成 `Value`；寄存器里照旧用 `Value` 枚举
- **属性表**：形状指针原子换；槽数组只在要长大时整块换新（CAS 换指针，旧数组本身就是 GC 对象，没人引用了就被回收）。
  属性写入 = 一次原子 store；加属性 = 形状迁移 + CAS
- **JS 语义**：两个线程同时写同一个属性，结果是其中一次写入（每个槽最后一个写入者生效），不会撕裂，也没有未定义行为

### P4 r8framework / 执行器

这一期不在 r8gc 里，列出来是为了说明全貌：

- `Shape`：`Rc` + `RefCell` 的迁移表 → `Arc` + 无锁迁移表（`PushList` 加按键查找）
- `Realm` 里共享的东西（`jobs`、正则缓存、`shape_root` 等）改成原子或无锁结构；`Realm::jobs` 接 r8taskqueue
- 每个工作线程一个 `Agent`（拿自己的 `Mutation`），共用同一个 `Realm` / 堆
- 内联缓存只存形状地址加槽号，读取时校验形状，本来就容得下并发

## 要定的事

1. **安全点算不算锁**：P2 里 mutator 碰到「请求回收」位时要等回收做完。mutator 之间从不互相等，只在回收的时候等回收方。
   不想等的唯一办法是并发搬对象，每次读指针都要经读屏障（Brooks 转发指针那一类），每次属性访问都要多付代价。**建议用安全点。**
2. **`RefLock` 的替代**：字段级原子加 `AtomicValue`（建议），还是整对象读-拷贝-CAS（RCU：实现简单，但每次写都要拷贝整个 `ObjectData`）。
3. **基础件放哪**：新开 crate `r8lockfree`（建议），还是放进 r8gc、由 r8taskqueue 依赖 r8gc。
