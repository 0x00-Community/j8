# 总体架构：一段 JS 的一生

## crate 分层

```text
                        j8（门面：按 feature 汇总、导出绑定宏与 runtime!）
                                        │
   ┌──────────────┬───────────────┬─────┴──────┬─────────────┬────────────┐
 j8jit        j8executor      j8builtins   j8platform    j8binding     j8napi
   │   ╲          │   ╲            │            │             │            │
   │    ╲     j8compiler ╲         │            │             │            │
   │     ╲        │       ╲        │            │             │            │
   │      └── j8bytecode ──┴───────┼────────────┼─────────────┼────────────┘
   │                               │            │             │
   └───────────────────────── j8framework ──────┴─────────────┘
                                   │
                    paragc · j8taskqueue · j8lockfree · j8macros
```

| crate | 职责 |
|---|---|
| `j8framework` | 核心框架层：定义各层要实现的 trait（`Platform`、`Engine`、`Invoke`、`Intrinsics`）和共用的结构体（`Isolate`、`Scope`、`Agent`、`Context` / `Realm`、`Value`、对象模型、事件循环）。JS 运算语义挂在 `Agent` 上。不依赖任何其他层 |
| `j8bytecode` | 字节码定义，与 V8 Ignition 逐字节一致；依赖根，不依赖其他 j8 crate |
| `j8compiler` | JS ⇒ 字节码（oxc 解析，代码生成照 V8 的 `BytecodeGenerator`） |
| `j8executor` | 字节码执行器：实现 `Engine`，带内联缓存、分片执行与 JIT 钩子 |
| `j8jit` | 推测式 JIT（x86-64 Unix）：实现执行器的 `Tier` |
| `j8builtins` | 标准内建对象：实现 `Intrinsics` |
| `j8platform` | `Platform` 的标准库实现：线程池、时钟、模块加载 |
| `j8binding` | Rust ⇄ JS 绑定的运行时（`FromJs` / `IntoJs`、宿主对象） |
| `j8napi` | Node-API：原生插件链接的 C 接口 |
| `j8taskqueue` | 无锁数据流任务队列（`no_std`），每个事件循环就是一个队列 |
| `j8lockfree` | 手写的无锁基础件（`no_std`） |
| `j8macros` | 过程宏：字节码表生成、堆类型、绑定宏 |

依赖方向的规矩：**各层只依赖 `j8framework`**（执行器另外依赖编译器与字节码，JIT 依赖执行器），`j8framework`
不依赖任何层，只依赖 paragc 与两个基础件。所以内建对象不知道执行器，平台不知道内建对象——它们都只对着
`j8framework` 里的 trait 写。

## trait 是接缝

`j8framework` 定义接口、各层实现，宿主把它们拼起来：

| trait | 谁实现 | 干什么 |
|---|---|---|
| `Platform`（父 trait `Pool`） | `j8platform::DefaultPlatform`、宿主自己的平台 | 线程池、时钟、`console.log` 输出、模块加载 |
| `Intrinsics` | `j8builtins::Builtins` | 往新 `Realm` 装全局对象与内建构造器 |
| `Engine` | `j8executor::Executor` | 执行入口：`eval`，可续跑的 `start` / `resume` |
| `Invoke` | 执行器 | 内建函数回调 JS（`map` 的回调、getter……） |
| `Tier` | `j8jit::Jit` | 执行器问「这个函数 / 循环要不要编」 |

## 一段 JS 的一生

以 `dune -p 'let s = 0; for (let i = 0; i < 1e6; i++) s += i; s'` 为例：

1. **宿主建环境。** dune 用 `j8::runtime!` 声明的运行时在一个事件循环线程上建 `Isolate`（一个 GC 堆）、
   `Executor`、`Context`（`Builtins` 装好全局对象），跑完 JS 序言（[搭一个宿主](embedding.md)）。
2. **编译。** 执行器拿到源码，交给 `j8compiler`：oxc 解析并做语义检查（早期错误在这里报 `SyntaxError`），
   作用域分析决定每个变量放寄存器、上下文槽还是全局对象，代码生成器照 V8 的 `BytecodeGenerator` 出字节码，
   结果是 `j8bytecode::Module`（一组 `SharedFunction`，每个带 `BytecodeArray`）。
   多个运行时跑同样的序言时，`CodeCache` 让同一份源码只编译一次（[执行器](executor.md)）。
3. **解释。** 执行器把字节码解码成「线程化代码」：每条指令带着跑它的处理函数，处理函数之间直接跳转。
   属性访问经内联缓存按 shape 命中。每跑一千来条指令检查一次事件队列，每一万条让出一次（分片），让 GC 有机会回收。
4. **升层。** 循环每次往回跳都给 profiler 计数。过阈值后执行器问 `Tier`：JIT 依据内联缓存的反馈把函数建成
   推测性 SSA 图（`s`、`i` 猜成 int32），优化后直接出 x86-64 机器码，从循环头切进去跑（OSR）。
5. **去优化。** 推测落空（比如 `s` 超过 int32），守卫跳到冷路径，`j8_deopt` 按记下的帧状态重建解释器帧，
   解释器接着跑；JIT 留下提示，下次编译那里改用 double。
6. **结束。** 结果是一个 `Value`（一个 NaN-boxing 的机器字），宿主拿到 `Global` 句柄，`ToString` 后打印。
   事件循环里没有剩下的宿主任务，进程退出。

接下来的章节按这条路逐段展开：[字节码](bytecode.md) → [编译器](compiler.md) → [执行器](executor.md) →
[JIT](jit.md)，然后是它们共同站着的[框架层](framework.md)、[对象模型](objects.md)与[堆](heap.md)。
