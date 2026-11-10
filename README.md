# r8

用 Rust 重写 V8。Cargo workspace，多 crate 组织。

## 结构

| crate | 职责 |
|---|---|
| `r8frame` | 核心框架层：定义 trait（`Platform`、`Engine`）与通用结构体 / 枚举（`Isolate`、`Context`、`HandleScope`·`Local`·`Global`、`Profiler`、`Heap`、`Value`、`BinaryOp`、`Error`……）。原则上只定义、实现归各层 crate；`Heap` / `Profiler` / 句柄表 / `Heap::binary` 目前仍有具体实现在这里，待拆 |
| `r8platform` | 实现 `Platform`：标准库线程与时钟（`DefaultPlatform`） |
| `r8bytecode` | JS ⇒ 字节码：累加器式寄存器字节码（`Insn` / `Function` / `Module`），`Module::try_from(&Source)` 编译 |
| `mercedes` | 实现 `Engine`：字节码执行器，**目前是最后一层** |

所有 JS 都先编成字节码再执行，不设 AST 解释层。

依赖方向：各层只依赖 `r8frame`（`mercedes` 另依赖 `r8bytecode`），`r8frame` 不依赖任何层。
嵌入方选一个 `Platform` 实现建 `Isolate`，选一个 `Engine` 交给 `Context::run`。

```rust
use mercedes::Mercedes;
use r8frame::{CreateParams, Isolate, Source};
use r8platform::DefaultPlatform;

let platform = DefaultPlatform::default();
let mut isolate = Isolate::new(&platform, CreateParams::default());
let mut cx = isolate.context();
let v = cx.run(&mut Mercedes, &Source { text: "1 + 2 * 3", file: "a.js" })?;
```

## 规划（未开工，只停留在设计）

- `jspass`（本地外部仓库 `../jspass`）：字节码下降到 MLIR 方言
- `tornado`：JIT 编译器与执行层调度器，按 `Profiler` 热度从 mercedes 升层，编不了就留在 mercedes

## 现状

字节码编译器目前只支持：表达式语句、数字 / 字符串 / 布尔 / `null` / `undefined` 字面量、括号、一元 `-`、`+ - * / %`（`+` 遇字符串拼接）。
其余写法报 `Error::Unsupported`，带 `文件:行:列`。

## 约定

- 子模块私有 `mod`，crate 根 `pub use` 平铺导出，⛔ `pub mod`
- ⛔ `static` 与 `'static` 生命周期（枚举里的 `&'static str` 名字除外）；后台任务用 `Task<'t>` 借用调用方数据
- 字符串反序列化后立即转枚举，⛔ 拿字符串做判断
- 转换走 `From` / `TryFrom`
- 单元测试只钉确定的事实（每次只跑一个，`timeout 15`）
