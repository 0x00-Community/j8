# r8

用 Rust 重写 V8。Cargo workspace，多 crate 组织。

## 结构

| crate | std | 职责 |
|---|---|---|
| `r8frame` | `no_std` | 核心框架层：定义 trait（`Platform`、`Engine`）与通用结构体 / 枚举（`Isolate`、`Scope`、`Context`、`Global`、`Value`、`JsString`·`JsObject`、`Profiler`、`BinaryOp`、`Error`……），实现归各层 crate |
| `r8platform` | std | 实现 `Platform`：标准库线程与时钟（`DefaultPlatform`） |
| `r8bytecode` | `no_std`（`compile` 要 std） | 累加器式寄存器字节码（`Insn` / `Function` / `Module`）；`compile` feature（缺省开）带 JS ⇒ 字节码编译器 `Module::try_from(&Source)`，用 oxc 解析 |
| `r8executor` | `no_std`（`compile` 要 std） | 字节码执行器 `Executor`，**目前是最后一层**；`compile` feature 下实现 `Engine` |

所有 JS 都先编成字节码再执行，不设 AST 解释层。
依赖方向：各层只依赖 `r8frame`（`r8executor` 另依赖 `r8bytecode`），`r8frame` 不依赖任何层。

### 堆与句柄

堆用外部实现 [gc-arena](https://docs.rs/gc-arena)（增量标记清除，品牌生命周期，无 `'static`）：

- `Isolate::enter(|scope| …)` 进入一次，`'gc` 是这次进入的品牌；闭包期间不回收，返回后按欠账增量回收
- `Value<'gc>` 就是 V8 的 Local：出不了闭包，编译期保证
- `Global` 是持久句柄（gc-arena `DynamicRoot`），跨多次 `enter` 存活，drop 即放掉

```rust
use r8executor::Executor;
use r8frame::{CreateParams, Isolate, Source};
use r8platform::DefaultPlatform;

let platform = DefaultPlatform::default();
let mut isolate = Isolate::new(&platform, CreateParams::default());
isolate.enter(|scope| {
    let cx = scope.context();
    let v = scope.run(cx, &mut Executor, &Source { text: "1 + 2 * 3", file: "a.js" });
});
```

验证 `no_std`：

```sh
cargo build --target riscv64gc-unknown-none-elf -p r8frame -p r8bytecode -p r8executor --no-default-features
```

## 规划（未开工，只停留在设计）

- `jspass`（本地外部仓库 `../jspass`）：字节码下降到 MLIR 方言
- `tornado`：JIT 编译器与执行层调度器，按 `Profiler` 热度从 r8executor 升层，编不了就留在 r8executor

## 现状

字节码编译器目前只支持：表达式语句、数字 / 字符串 / 布尔 / `null` / `undefined` 字面量、括号、一元 `-`、`+ - * / %`（`+` 遇字符串拼接）。
其余写法报 `Error::Unsupported`，带 `文件:行:列`。

## 约定

- 尽可能 `no_std`（`core` + `alloc`），要 std 的部分放在 feature 后面

- 子模块私有 `mod`，crate 根 `pub use` 平铺导出，⛔ `pub mod`
- ⛔ `static` 与 `'static` 生命周期（枚举里的 `&'static str` 名字除外）；后台任务用 `Task<'t>` 借用调用方数据
- 字符串反序列化后立即转枚举，⛔ 拿字符串做判断
- 转换走 `From` / `TryFrom`
- 单元测试只钉确定的事实（每次只跑一个，`timeout 15`）
