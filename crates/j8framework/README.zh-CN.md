# j8framework

[English](README.md) · **简体中文**

[j8](https://github.com/0x00-Community/j8) 的核心框架层：定义其余各层要实现的 trait 和它们共用的结构体 / 枚举，不依赖任何其它 j8 层。

## 里面有什么

| 名字 | 作用 |
| --- | --- |
| `Platform` / `Pool` | 宿主借给引擎的线程与时钟（`j8platform` 实现） |
| `Isolate` | 独立的 JS 世界：一个 GC 堆（[paragc](https://crates.io/crates/paragc)）加一个 profiler |
| `Scope` | 带着一个 `Context` 和执行器进入 isolate 的一次；里面的值带 `'scope` 品牌，出不了这次进入；全部 JS 运算语义挂在它上面，回调 JS 经 `Invoke` |
| `ScopeGlobal<'p, B>` | 跨多次 `enter` 存活的根；`B` 标明类型（`ValueBrand`、`ContextBrand`） |
| `Context` | 执行上下文：全局对象与内建原型（`Intrinsics`，由 `j8builtins` 实现） |
| `Host` / `HostObject` | 放在 JS 对象里、由回收器追踪的 Rust 值 |
| `Value` / `JsObject` / `JsString` … | NaN-boxing 的值与堆对象 |

### GC 层

堆是 paragc，只用它的公开接口。`gc.rs` 在上面补 JS 引擎要的东西：

- `GcCell`：无锁的 `RefCell`，数据放在 paragc `Slot` 指着的快照里（年轻快照原地改，旧的先挪进新快照再改，写屏障因此看得见）
- `ValueCell`：一个 NaN-boxing 的 `Value`（原子字 + `Slot`）
- `Cow`、`GcQueue`、`Static`，以及 `#[derive(Collect)]`（在 `j8macros`）

对象把热路径要碰的东西放在 cell 外面：原型、属性（shape 加 GC 堆里的值存储，见 `shape.rs` / `store.rs`）、数组元素、
函数与正则的不可变数据。规矩：值进堆之后才能写它的槽；堆类型都是 `Send + Sync`；堆只做停顿式回收。

## 用法

```rust
use j8builtins::Builtins;
use j8executor::Executor;
use j8framework::{CreateParams, Isolate, ContextBrand, Source};
use j8platform::DefaultPlatform;

let platform = DefaultPlatform::default();
let mut isolate = Isolate::new(&platform, CreateParams::default());
let executor = Executor::default();
let cx = isolate.create_context(&Builtins);
let result = executor.eval(&mut isolate, &cx, &Source { text: "1 + 2 * 3", file: "a.js" })?;
```

## 许可

MIT，随 [j8](https://github.com/0x00-Community/j8) 一起。
