# j8 是什么

j8 是**用 Rust 从头写的 JavaScript 引擎**。除了解析器用 [oxc](https://oxc.rs)，从字节码、执行器、JIT 到垃圾回收都是自己写的，
下面没有 C++ 引擎。

它照着 V8 的分层来：

```text
JS 源码 ─ oxc 解析 ─ j8compiler ─▶ V8 Ignition 字节码 ─ j8executor 解释 ─┐
                                                                         ├─▶ 结果
                       热函数 / 热循环 ─ j8jit 推测式编译 ─▶ x86-64 机器码 ─┘
```

## 设计取向

| | |
|---|---|
| 🦀 **全 Rust** | 解析用 oxc，其余都是自己的代码。 |
| 🧱 **字节码先行** | 不设 AST 解释层：所有 JS（包括 `eval`、`Function`）都先编成字节码再跑。 |
| 🔣 **V8 的字节码** | 字节码表、操作数编码、寄存器与帧布局都与 V8 Ignition 逐字节一致，表格直接从 V8 头文件生成。 |
| 🧬 **推测式 JIT** | 依据内联缓存收集的反馈，把字节码建成推测性的 SSA 图：内联、逃逸分析、循环不变量外提、OSR、去优化。 |
| ♻️ **paragc 堆** | Immix 式块 / 行堆，年轻回收 + 老对象引用计数，标记与清扫并行；`'gc` 品牌生命周期在编译期保证句柄不逃逸。 |
| 🚫 **没有全局状态** | 不用 `static` 与 `'static`；后台任务借用调用方的数据，多个 Isolate 共用一个线程池。 |
| 🧩 **可嵌入** | `#[j8::class]` / `#[j8::object]` / `#[j8::function]` 一行把 Rust 类型绑成 JS，`j8::runtime!` 拼出一个运行时。 |

## 主线：test262

j8 的主线是 [test262](https://github.com/tc39/test262) 通过率。基线 `tests/test262.passing` 当前是 **42,443** 条用例
（非严格 + `onlyStrict`；不含 Temporal、intl402 等，见[路线图](roadmap.md)），每次提交都要零回归，开着 JIT 也跑同一份基线。

## 仓库里有什么

| 目录 | 内容 |
|---|---|
| `crates/` | 引擎本身，十几个 crate（见[总体架构](architecture.md)） |
| `src/` | 门面包 `j8`：按 feature 汇总各 crate，导出绑定宏 |
| `dune/` | 对标 Node 与 Bun 的运行时，附带 `dunx`（对标 npx / bunx） |
| `samples/` | 样例宿主与工具：workerd、jumpheap、j8bcdbg、三组基准 |
| `tests/` | 集成测试与 test262 运行器 |
| `tools/` | `v8bytecodes.py`：从 V8 头文件生成字节码表 |
| `mdbook/` | 本书 |

## 发布

全部 crate 发在 crates.io：门面包 `j8`（0.4.0）按 feature 选组件，子 crate（`j8bytecode`、`j8compiler`、`j8executor` ……）
也单独发布。dune 以 `j8dune` 的名字发布（`dune` 已被占用）。许可证 MIT。
