# 框架层：Isolate、Agent 与值

`crates/j8framework` 是核心框架层：定义其余各层要实现的 trait 和它们共用的结构体 / 枚举，**不依赖任何其他 j8 层**。
具体实现在各层 crate 里（比如 `j8platform` 实现 `Platform`）。

## 核心类型

| 名字 | 作用 | 对应 V8 |
|---|---|---|
| `Platform` / `Pool` | 宿主借给引擎的线程池与时钟 | `v8::Platform` |
| `Isolate` | 独立的 JS 世界：一个 GC 堆（paragc）加一个 profiler | `v8::Isolate` |
| `Scope` | 进入 isolate 的一次；里面的值带 `'scope` 品牌，出不了这次进入 | `HandleScope` |
| `Value<'scope>` | 一个 JS 值，就是 V8 的 `Local`，出不了闭包 | `Local<Value>` |
| `ScopeGlobal<'p, B>` | 根（paragc `ScopeGlobal`），跨多次 `enter` 存活，drop 即放掉；`B` 标明持有的类型（`ValueBrand`、`ContextBrand` …） | `Global<T>` |
| `Context` | 执行上下文：全局对象与内建原型（`Intrinsics` 由 j8builtins 实现） | `Context` |
| `Agent` | 一次运行的上下文；全部 JS 运算语义挂在它上面，回调 JS 经 `Invoke` | — |
| `Profiler` | 调用 / 回跳计数器 | — |
| `Host` / `HostObject` | 放在 JS 对象里、由回收器追踪的 Rust 值（见[绑定](bindings.md)） | cppgc 对象 |

## 进入 isolate

```rust,ignore
let platform = DefaultPlatform::default();
let mut isolate = Isolate::new(&platform, CreateParams::default());
let executor = Executor::default();
let cx = isolate.enter(|scope| scope.mc.stash::<ContextBrand>(scope.context(&Builtins)));   // ScopeGlobal<ContextBrand>
let result = executor.eval(&mut isolate, &cx, &Source { text: "1 + 2 * 3", file: "a.js" })?;
isolate.enter(|scope| {
    let v = scope.local(&result);       // ScopeGlobal ⇒ Value<'scope>，只在这个闭包里有效
});
```

- `Isolate::enter(|scope| …)` 进入一次，`'scope` 是这次进入的**品牌**。闭包期间不回收；返回后如果分配得够多就回收
  （通常是年轻回收，每隔几轮一次完整回收）。
- `Value<'scope>` 出不了闭包，**编译期保证**——这是 j8 用 Rust 生命周期替代 V8 `HandleScope` 的方式。
- 要跨 `enter` 保留一个值，就挂到根上：`agent.global(v)` 得到 `ScopeGlobal<'p, ValueBrand>`，`scope.mc.stash::<B>(x)`
  可以把任何堆对象挂上（上下文用 `ContextBrand`）；`local` / `mc.fetch` 读回。
- 执行器（`j8executor::Executor<'p>`）跑脚本：`eval` / `eval_module`，或者分片的 `start` / `resume`。事件循环也在
  j8executor 里（[事件循环](event-loop.md)）。

## 值：一个 NaN-boxing 的机器字

`Value` 是一个 64 位字（`ValueBits`）：

- **数**就是它自己的 `f64` 位（所有 NaN 都规范成 `CANONICAL_NAN`）；
- **其余**都在负 NaN 空间里：高 16 位是 `0xFFF0 | (tag + 1)`，低 48 位是载荷——布尔值，或 GC 地址（用户态地址装得进 48 位）。

`ValueTag` 的取值：`UNDEFINED`、`NULL`、`BOOL`、`NUMBER`、`UNINIT`（TDZ 里还没初始化的绑定，只出现在寄存器与环境槽里，
读写前被检查掉，永远到不了 JS）、`LAZY`（首次读才计算的属性值）、`STRING`、`SYMBOL`、`BIGINT`、`OBJECT`、`ACCESSOR`、`CONTEXT`。

判断「是不是数」「是不是对象」都是一次比较，不必完整解码。寄存器、累加器、环境槽、属性槽存的都是同一个字，
所以 JIT 的机器码直接读写它，`ValueCell` 原样存它。

```rust,ignore
let v = Value::Number(1.5);
match v.kind() {
    ValueKind::Number(n) => …,
    ValueKind::Object(o) => …,
    _ => …,
}
```

## Agent：运算语义

`Agent` 是「一次运行」的上下文，持有当前 realm、堆的 mutator 与引擎的 `Invoke`。规范里的抽象操作都是它的方法
（`ops.rs`）：类型转换（ToNumber、ToString、ToPrimitive……）、运算符（`BinaryOp`、`CompareOp`、`UnaryOp`）、
属性访问（沿原型链，经[对象的内部方法](objects.md)）。执行器、内建对象、绑定代码共用这一套，所以语义只写一次。

```rust,ignore
let s = a.to_string(v)?;
let f = a.get_named(obj, "then")?;
let r = a.call(f, this, &[arg])?;
let err = a.throw(ErrorKind::Type, "x is not a function");   // 返回 Error，宿主函数 `?` 出去即抛出
```

内建函数与宿主函数的签名统一是 `fn(&mut Agent, this, &[Value]) -> Result<Value, Error>`。

反复调用同一个函数的内建（`map`、`filter`、`reduce`、`sort` 的比较函数……）先用 `a.callback(f, this)` 解析一次，
循环里调用返回的 `Callback`。引擎经 `Invoke::callback` 填好它：开了 JIT 时，不超过三个形参的普通函数直接进编译好的机器码
（经函数的跳板，之后重编或去优化都能跟上）；其余情况退回 `Agent::call`。

```rust,ignore
let cb = a.callback(f, this_arg);
for (k, v) in items {
    let r = cb.call(a, &[v, k.into(), o.into()])?;
}
```

## Error

| 变体 | 含义 |
|---|---|
| `Error::Exception` | JS 抛了；值在待处理槽里（`agent.take_exception()`），照 V8 |
| `Error::Parked(ticket)` | JS 抛了，宿主代码把值暂存进 isolate（`agent.park()`），留到后面某一轮再处理；`agent.unpark(ticket)` 放回待处理槽 |
| `Error::Syntax(msg)` | 源码解析不了（含早期错误） |
| `Error::Unsupported(msg)` | j8 还不支持的写法，带 `文件:行:列` |
| `Error::Terminated` | 被 `IsolateHandle::terminate` 中止 |

## 中止执行

`IsolateHandle::terminate` 从别的线程中止 JS（V8 的 `TerminateExecution`）：执行层在检查点（函数入口、回跳；JIT 机器码在回跳）
看到请求后抛 `Error::Terminated`。它不是 JS 异常，`catch` / `finally` 都接不住，一路传到最外层；传出最外层时自动复位，
isolate 接着能用。

## 其他部件

| 文件 | 内容 |
|---|---|
| `promise.rs` | Promise 与微任务：resolve / reject、`then` 反应、驱动 async 协程、排空队列 |
| `module.rs` | 模块记录、导入导出条目、`ResolvedBinding` |
| `proxy.rs` | Proxy：内部方法入口分派到 trap，带不变式检查 |
| `typed.rs`、`backing.rs` | TypedArray 元素；ArrayBuffer 的字节放在哪（自己的、宿主映射的、几个 isolate 共享的） |
| `text.rs` | 字符串（见[对象模型](objects.md)） |
| `parallel.rs` | 内建函数的并行：大数据时把纯数据的活切块放到平台线程池上（fork–join） |
| `layout.rs` | 机器码要直接读写的堆对象字节偏移 |
| `lazy.rs` | 首次读才计算的属性（V8 函数 map 上的 `AccessorInfo`：建闭包时不造 `name`） |
| `private.rs` | 类的私有成员 `#x` |
