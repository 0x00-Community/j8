<a id="top"></a>
<div align="center">

# j8

### 用 Rust 重写 V8.
**字节码解释器 + 经 MLIR 的 JIT + 并行回收的堆 ([paragc](https://crates.io/crates/paragc)).**

[![Rust](https://img.shields.io/badge/Rust-100%25-orange?style=for-the-badge&logo=rust)](https://www.rust-lang.org)
[![test262](https://img.shields.io/badge/test262-42%2C443%20%E9%80%9A%E8%BF%87-success?style=for-the-badge)](#-test262)
[![License](https://img.shields.io/badge/License-MIT-brightgreen?style=for-the-badge)](#-许可证)

[![ECMAScript](https://img.shields.io/badge/ECMAScript-%E6%A0%B8%E5%BF%83%E8%AF%AD%E8%A8%80-F7DF1E?style=flat-square&logo=javascript&logoColor=black)](#-现状)
[![JIT](https://img.shields.io/badge/JIT-x86--64%20%C2%B7%20AArch64-262D3A?style=flat-square&logo=llvm)](#-jit第一档)
[![GC](https://img.shields.io/badge/GC-paragc%20%C2%B7%20%E5%B9%B6%E8%A1%8C%20%C2%B7%20%E6%97%A0%E9%94%81-8A2BE2?style=flat-square)](#-堆与句柄)

[🇺🇸 English](README.md) · **🇨🇳 简体中文**

</div>

---

## ⚡ j8 是什么

j8 是**用 Rust 从头写的 JavaScript 引擎**, 照着 V8 的分层来: 所有 JS 先编成累加器式字节码 (同 V8 Ignition),
由执行器解释, 热函数和热循环交给 JIT 出 x86-64 或 AArch64 机器码. 堆是 [paragc](https://crates.io/crates/paragc), 外面包一层 `j8framework` 的 GC 层.

🎯 **主线:** test262 通过率. 核心 ECMAScript 按 [PLAN.md](PLAN.md) 逐阶段补齐, 每一步跑基线零回归.

🧩 **可嵌入:** `#[j8::class]` / `#[j8::object]` / `#[j8::function]` 一行把 Rust 类型绑成 JS; 自带两个样例: 分 Web 标准层
与 Node 兼容层的运行时 `dune`, 以及 Cloudflare Worker 式运行时 `workerd`. 照样搭自己的宿主: 先看 [EMBEDDING.md](EMBEDDING.md)（各部分怎么拼, 英文）,
宏的逐项说明在 [crates/macros/USAGE.md](crates/macros/USAGE.md).

## 🔥 凭什么不一样

| | |
| --- | --- |
| 🦀 **全 Rust** | 解析用 oxc, 其余从字节码到 GC 都是自己写的. 没有 C++ 引擎在下面. |
| 🧱 **字节码先行** | 不设 AST 解释层: 所有 JS (包括 `eval`、`Function`) 都先编成字节码, 而且就是 V8 Ignition 的字节码, 逐字节一致 (表格从 V8 头文件生成). |
| 🧬 **推测式 JIT** | 依据内联缓存的反馈把热字节码建成推测性 SSA 图 (内联、逃逸分析、循环不变量外提、OSR、去优化), 直接出 x86-64 或 AArch64. |
| ♻️ **paragc 堆** | Immix 式块 / 行堆: 年轻回收 + 老对象引用计数、稀疏块疏散, 标记与清扫并行, 无锁; `'scope` 品牌生命周期, 编译期保证句柄不逃逸. |
| 🚫 **没有 `'static`** | 不用全局状态; 后台任务借用调用方数据, 多个 Isolate 共用一个线程池. |

## 🏆 数字

| ✔️ 项目 | 结果 |
| --- | --- |
| 📜 test262 基线 | **42,443** 条通过 (非严格 + `onlyStrict`; 不含 Temporal / intl402 等, 见 PLAN.md), 每次提交零回归 |
| 🧮 JIT | `J8_TEST262_JIT=1` 开着 JIT (每个函数第一次调用就编) 跑同一份基线 |
| ⏱️ 对 V8 jitless | 11 个基准几何平均耗时 **0.6×**, 校验和三种引擎全一致 |
| 🚄 对 V8 (带 JIT) | 几何平均耗时 **1.7×**; 差距在内建函数 (array、sort、string、collections、json、regexp), 详见 [benchmark.md](samples/benchv8/benchmark.md) |

## 📦 安装

```sh
cargo add j8                                    # 缺省 feature `runtime`: 执行器 + 内建对象 + 标准平台
cargo add j8 --features jit                     # 再加 JIT (x86-64 Unix 和 Windows、AArch64 Linux)
cargo add j8 --features crossheap               # Linux: 凭签名把堆对象共享给别的进程 (见 EMBEDDING.md 第 9 节)
cargo add j8 --no-default-features --features compiler   # 只要 JS ⇒ 字节码
```

子 crate (`j8bytecode`、`j8compiler`、`j8executor` …) 也单独发了，但一般走根包加 feature。接宿主看 [EMBEDDING.md](EMBEDDING.md)。

## 🚀 快速开始

```sh
# 🟢 dune, 对标 Node, 兼容 Node (加 --web 只要 Web 标准接口): 跑脚本 / 求值 / REPL
cd dune
cargo run --release -- examples/node.js
cargo run --release -- --web examples/web.mjs
cargo run --release -- -p '[1, 2, 3].map(x => x * 2)'
cargo run --release                             # REPL (.help / .exit)
cargo run --release -- --jit a.js               # 开 JIT
cargo run --release -- x cowsay hi             # dune x (dunx 是 dune 的链接): 跑包里的可执行文件, 没装就从 npm 装进缓存

# ☁️ Cloudflare Worker 式: ES 模块 + Durable Object
cd samples/workerd && cargo run -- examples/counter.js --bind COUNTER=Counter --request "GET /c/inc?name=a"

# 🔬 列出字节码 (照 V8 的 --print-bytecode)
cd samples/j8bcdbg && cargo run -- compile -e 'let s = 0; for (let i = 0; i < 10; i++) s += i; s'

# 📜 test262 回归 (先 git submodule update --init)
cargo test --release --test test262

# 🥊 对 V8 的基准 (下载 rusty_v8 预编译库)
cd samples/benchv8 && cargo run --release
```

## 🧱 结构

`dune/`（对标 Node 的运行时）和 `samples/` 下的样例各自成包, 不在 workspace 里, 像嵌入方一样按版本依赖发布的 `j8`.

| crate | std | 职责 |
|---|---|---|
| `j8framework` | std | 核心框架层：定义 trait（`Platform`、`Invoke`、`Intrinsics`）与通用结构体 / 枚举（`Isolate`、`Scope`、`Scope`、`Context`、`ScopeGlobal`、`Host`、`Value`、`JsString`·`JsObject`、`Profiler`、`BinaryOp`、`Error`……），JS 运算语义挂在 `Scope` 上，实现归各层 crate。 |
| `j8`（仓库根 `src/`） | std | 聚合导出：`#[j8::class]` / `#[j8::object]` / `#[j8::function]`（宏在 j8macros，见 `crates/macros/USAGE.md`）与 j8binding 的 trait；其余 crate 按 feature 导出（缺省 `runtime` = `j8::executor` + `j8::builtins` + `j8::platform`，另有 `bytecode` / `taskqueue` / `lockfree` / `jit` / `full`），每个子 crate 也单独发在 crates.io |
| `j8binding` | std | Rust ⇄ JS 绑定的运行时：`FromJs` / `IntoJs`、宿主对象、`Class` / `Object` |
| `j8builtins` | std | 实现 `Intrinsics`：往 `Context` 装 Object / Function / Array / String / Number / Boolean / Math / Error 系列 / `console` / `parseInt` 等 |
| `j8platform` | std | 实现 `Platform`（`DefaultPlatform`）：后台任务跑在 moremath `ThreadMulti` 线程池上（起一次复用、任务可借用调用方数据，经 `j8framework::Pool`），时钟用 `Instant` |
| `j8bytecode` | std | 字节码，依赖根（不依赖其他 j8 crate）：与 V8 Ignition 逐字节一致（`Bytecode` / `OperandType` … 由 `v8.json` 生成，`tools/v8bytecodes.py` 从 V8 头文件写出），`Encoder` / `Decoder`，`SharedFunction` / `BytecodeArray` / `Module` |
| `j8compiler` | std | JS ⇒ 字节码（`Module` 实现 `j8compiler::Compile`：脚本、模块、`eval` / `Function`），用 oxc 解析 |
| `j8taskqueue` | `no_std` | 任务队列：`ITask`（`recursable` / `depend` / `exec`）与 `ITaskPoll`（`add` / `run`）；无锁数据流调度（手写 MPMC 就绪队列、原子依赖计数），依赖做完的任务立刻在宿主给的 `Pool` 上并行跑（或就在调用线程上跑）、没有轮次屏障；`TaskId::family` 并查集找爹、`cancel` 整家取消；配额按字节码条数（`Quota` / `Flow::Yield`、每次 run 的预算）；取不到活的线程直接离开、不自旋。每个 `j8executor::EventLoop` 就是一个 `Queue`：脚本、兑现、宿主 future 的 poll，脚本每让出一次就检查一次队列 |
| `j8macros` | proc-macro | 过程宏：`enums!("x.json")` 从 JSON 生成枚举（定义、`Display`、按字段类型取 `&mut`）、堆类型的 `#[derive(Collect)]`（paragc `Trace`）、绑定宏，用法见 `crates/macros/USAGE.md` |
| `dune` | std | 对标 Node 的运行时，分两层：`src/web/`（Web 标准接口：`console`、定时器、`URL`、`TextEncoder`、`EventTarget` / `AbortController`、`fetch`、`structuredClone`、`crypto`……）和叠在上面的 `src/node/`（`process`、`require` / ES 模块、`Buffer`、`fs`、`path`、`os`、`events`、`util`、`assert`、`child_process`……）；碰机器的部分是 `#[j8::object]` / `#[j8::class]` 绑定的 Rust，接口本身是 JS prelude；`dune a.js`、`dune --web a.mjs`、`dune -e` / `-p`、REPL，详见其 README |
| `samples/workerd` | std | 用 j8 加载 Cloudflare Worker（ES 模块 `export default { fetch }`）与 Durable Object：`workerd examples/counter.js --bind COUNTER=Counter --request "GET /c/inc?name=a"` 或 `--port 8787` 起 HTTP 服务；Fetch API 子集与 DO 在 JS 序言里，存储是 `#[j8::class]` 绑定的 Rust KV |
| `samples/benchv8` | std | 同一组 JS 基准在 j8 与 V8（rusty_v8 152，带 JIT / `--jitless`）上的耗时对比：`cd samples/benchv8 && cargo run --release`（会下载 V8 预编译库） |
| `samples/j8bcdbg` | std | 字节码调试命令行（clap）：`j8bcdbg compile <file.js \| ->` / `j8bcdbg compile -e '<js>'` 按函数列出字节码，格式照 V8 的 `--print-bytecode`（`-o` 写文件） |
| `j8executor` | std | 字节码执行器 `Executor<'p>`，**目前是最后一层**（`eval`，可续跑的 `start` / `resume`）；`EventLoop` 是一个 JS 事件循环（一个 isolate、执行器与一个上下文，宿主异步活），`EventLoopSet` 把许多事件循环钉在少数 loop 线程上跑，带负载均衡（放置、工作窃取、`rebalance`、`split`）与统一配额；固定带 JS ⇒ 字节码编译器（脚本、模块、`eval` / `Function` 都先编成字节码） |

所有 JS 都先编成字节码再执行，不设 AST 解释层。
依赖方向：各层只依赖 `j8framework`（`j8executor` 另依赖 `j8compiler`；`j8bytecode` 是根，`j8framework` 与 `j8compiler` 依赖它），`j8framework` 不依赖任何层，只依赖基础件 `j8lockfree` 与 `j8taskqueue`。

### 🧹 堆与句柄

堆是 [paragc](https://crates.io/crates/paragc)，只用它的公开接口；`j8framework` 的 GC 层在上面补 JS 引擎要的东西：`GcCell`
（无锁的 `RefCell`，数据放在 paragc `Slot` 指着的快照里：上次回收后新分配的快照原地改，旧的先挪进一份新快照再改，写屏障因此看得见）、
`ValueCell`（一个 NaN-boxing 的 `Value`：原子字 + `Slot`）、`Cow`、`GcQueue`、`Static`，以及 `#[derive(Collect)]`（在 j8macros；
`#[collect(require_static)]` 跳过一个字段或整个类型）。品牌生命周期，无 `'static`。

- 回收只在 `enter` 之外、所有 `ScopeLocal` 都挂在根上时发生，对象会被搬：⛔ 把 `ScopeLocal` 或对象地址存进 `Trace` 不遍历的地方（`Static`、旁路缓存）；
  按地址哈希的东西（Map / Set 的索引）在 `relocate` 里更新（见 `Collection`）
- ⛔ 给还没进堆的值写 `Slot` / `ValueCell` / `Cow`（写屏障记的是槽地址）：先分配进堆再填（见 `Scope::context`）
- 堆只做停顿式回收（`collect_if_needed` / `collect`，内部并行）；`GcCell` 原地改年轻快照不经写屏障，所以 ⛔ `mark_concurrently`
- 堆里的类型都是 `Send + Sync`（paragc 的规矩）：宿主数据是 `GcCell` 里会被追踪的 `Box<dyn HostObject + Send + Sync>`，
  rope 用 `Arc`
- 堆有自己的回收线程（数目同平台线程池）；`Platform` 的父 trait `Pool` 给并行内建函数用

- `Isolate::enter(&cx, &executor, |scope| …)` 进入一次，`'scope` 是这次进入的品牌；闭包期间不回收，返回后分配够多就回收（通常是年轻回收，每隔几轮一次完整回收）
- `Executor::eval` 拿 `&mut Isolate` 自己分片进入：每片最多一万条指令，停下时执行状态挂到根上、`enter` 返回时回收一段再接着跑；执行环境用 `ScopeGlobal<'p, ContextBrand>` 跨片保留（宿主函数里重入的 JS 在当前片里跑完）
- `Value<'scope>` 就是 V8 的 Local：出不了闭包，编译期保证
- `ScopeGlobal<'p, B>` 是根（paragc 的），跨多次 `enter` 存活，drop 即放掉；`B` 标明类型（`ValueBrand`、`ContextBrand`）
- 抛出的值照 V8 放在待处理异常槽里（`Error::Exception`，`scope.take_exception()`）

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
isolate.enter(&cx, &executor, |scope| {
    let v = scope.local(&result);
});
```

## 🧬 JIT

`dune --jit a.js` 开 JIT；`J8_TEST262_JIT=1 cargo test --release --test test262` 开着 JIT（每个函数第一次调用就编）跑基线。

- `j8jit` 实现执行器的 `Tier`。热函数（缺省 64 次调用）或热循环依据内联缓存看到的东西建成推测性 SSA 图：数先当 int32、小函数内联、对象逃逸前保持虚拟、上下文槽留在值里
- 图直接出 x86-64 或 AArch64 机器码（线性扫描寄存器分配，然后指令选择），在解释器的帧上跑；三种入口：解释器调用、循环中途切入（OSR）、已编译代码之间不建解释器帧的调用
- 守卫失败就去优化：`j8_deopt` 重建解释器帧（含内联掉的），给下次编译留提示（用 double、走通用路径、不内联）；一个函数去优化 24 次后一直解释
- 仍导出 `Compiler` / `Backend`（MLIR `llvm` 方言经 [mlirpass](https://crates.io/crates/mlirpass)，再由 j8jit 自己的 x86-64 后端出机器码）

## 🔣 字节码

V8 Ignition 的字节码，逐字节一致，对齐 V8 15.2.124：累加器式寄存器机（`acc = reg op acc`），操作数小端、带 `Wide` / `ExtraWide` 放宽，寄存器操作数是 V8 的帧槽号（r0 是 −7，receiver 是 +2）。表格从 V8 头文件生成（`tools/v8bytecodes.py` ⇒ `crates/j8bytecode/v8.json` ⇒ `j8macros::bytecodes!`），⛔ 手改。`samples/j8bcdbg` 按 V8 `--print-bytecode` 的格式列出字节码。

- 变量照 V8 的放法：寄存器、被捕获的绑定进上下文槽、顶层 `var` 在全局对象上、顶层 `let` / `const` 在脚本上下文里、`with` / 宽松模式直接 eval 下按名字查
- 执行器把每个函数解码一次成线程化代码（nightly 下处理函数之间尾调用），用显式帧栈（深度上限 10000，超了抛 RangeError），异常查字节码的处理表

`j8executor` 里的 `javascript_semantics` 测试是一张逐条确定的 JS 语义表。完整讲解见[本书](https://j8-mdbook.jabberwocky238.workers.dev/zh-CN/)。

## 📋 现状

支持：

- 字面量（数 / 字符串 / 布尔 / `null` / 模板字符串）、对象字面量（含方法简写、计算键）、数组字面量
- 全部算术、比较（`==` / `===` / 字符串按 UTF-16 比）、位运算、`**`、`typeof` / `void` / `!` / `~`、`&&` / `||` / `??`、`?:`、逗号
- 赋值与复合赋值（含 `||=` / `&&=` / `??=`）、`++` / `--`，目标可以是名字或属性
- `var` / `let` / `const`（块级作用域、对 `const` 赋值抛 TypeError）、函数声明提升
- `if` / `while` / `do-while` / `for` / `switch` / 带标签的 `break` / `continue` / `return` / `throw`
- 函数声明 / 表达式 / 箭头函数（词法 `this`）、闭包、方法调用的 `this`
- 属性读写（数组下标与 `length`、字符串下标与 `length`、原型链读）
- `try` / `catch` / `finally`（`finally` 里的 `return` / `break` / `continue` 与穿过 `finally` 的跳转都按规范）、Error 对象（`Error` / `TypeError` / `ReferenceError` / `RangeError` / `SyntaxError`）
- 内建对象（j8builtins）与宿主函数、`console.log`（经 `Platform::print`）、ToPrimitive（`valueOf` / `toString`）
- `new`、类（构造器、方法、静态方法、getter / setter、`extends` / `super(...)` / `super.m()`、实例与静态字段，可继承宿主构造器如 `Error`）、对象字面量的方法与 getter / setter
- 属性特性（可枚举 / 可写 / 可配置）、`Object.defineProperty` / `freeze`、`instanceof` / `in` / `delete`
- 剩余 / 默认参数、展开（数组 / 调用 / `new` / `super` / 对象）、解构（声明 / 赋值 / 形参 / `catch`，含默认值与剩余）、可选链 `?.`
- Symbol（含 `Symbol.iterator` / `toPrimitive` / `hasInstance`）、迭代协议、`for-of`（`break` / `return` / 抛出时关迭代器）/ `for-in`
- 生成器（`yield` / `yield*`，`return()` / `throw()` 经过 `finally`）、Promise（`then` / `catch` / `finally` / `all` / `race`）、`async` / `await`，脚本跑完后排空微任务队列
- 正则字面量与 `RegExp`（regress 引擎，按 UTF-16 码元匹配；`exec` / `test`、`g` / `y` 的 `lastIndex`、具名分组）与字符串的 `match` / `matchAll` / `replace` / `replaceAll` / `search` / `split`
- BigInt 字面量与运算（任意精度，与 Number 混算抛 TypeError、可比较、`BigInt()` / `toString(radix)`）
- 暂时性死区（`let` / `const` / `class` 与派生类 `super()` 前的 `this`；能证明已初始化的使用处编译期省掉检查）、脚本作用域（顶层 `let` / `const` / `class` 跨脚本共享、不挂在全局对象上）

- 类的静态块 / 私有名（字段、方法、访问器、`#x in o`）/ 计算字段名、async 生成器 / `for await`、标签模板（按调用点缓存）、`new.target`、`arguments`
- 动态代码：全局 `Function` / `eval`（间接）、生成器 / async 函数构造器；早期错误（oxc_semantic 检查 + 正则字面量解析时校验）编译期报 SyntaxError
- 严格模式（指令序言 / 类体 / 外层继承）：非严格调用的 `this` 换成全局对象或包装对象，严格模式下失败的赋值 / 删除抛 TypeError、给没声明的名字赋值抛 ReferenceError、`arguments.callee` 抛 TypeError；严格代码里的 `eval(…)` 按直接 eval 跑（代码严格、`var` 留在自己里面）

- 属性模型按规范的对象内部方法（ValidateAndApplyPropertyDescriptor、数组空洞与按元素的特性、`length` 只读、不可扩展、字符串包装对象的下标），`Object` / `Array` 的全部方法（Array 方法对类数组通用、species）
- Date、RegExp 的 Symbol 协议、String / Number / Math / Symbol / URI 函数补全、Promise 子类化与组合器、WeakRef / FinalizationRegistry（强引用）
- ArrayBuffer / SharedArrayBuffer / DataView / TypedArray（含 Float16、长度跟随可调大小缓冲）/ Atomics
- Proxy / Reflect、Iterator 助手、Set 方法（union 等）、Map.groupBy
- 宿主函数区分能否 `new`，内建构造器按 `new.target` 定原型（`class X extends Map` 等）

还不支持（报 `Error::Unsupported`，带 `文件:行:列`）：模块的顶层 await、形参表达式里 eval 的 `arguments` 冲突规则。test262 基线见 `tests/test262.passing`，补齐计划见 PLAN.md。

已知偏差：宿主函数里重入的 JS（如 `map` 的回调）在当前片里跑完，期间不回收（`Function.prototype.call` / `apply` 由执行器直接展开成普通调用，不算重入）。

## 📜 test262

`tests/test262` 是 tc39/test262 子模块（浅克隆，`git submodule update --init` 拉取），运行器 `tests/test262.rs` 是根包的测试（`harness = false`，不发布）：

```sh
cargo test --release --test test262 -- language/expressions/addition built-ins/Math   # 跑这些目录
J8_TEST262_JIT=1 cargo test --release --test test262                                   # 开 JIT（每个函数第一次调用就编）对拍
J8_TEST262_VERBOSE=1 cargo test --release --test test262 -- <路径>                    # 逐条列出没过的原因
J8_TEST262_UPDATE=1 cargo test --release --test test262 -- <路径>                     # 结果写回基线
cargo test --release --test test262                                                   # 只跑基线里的用例，掉了就失败
```

- 每个用例一个子进程、10 s 超时；结果分 pass / fail / unsupported（j8 报 `Error::Unsupported`）/ skip / timeout / crash，按目录汇总
- 基线 `tests/test262.passing`：过了的用例一行一个；不带参数时只跑它做回归检查
- 只跑非严格模式（`onlyStrict` 跑严格）；`module` 跳过；没有 `$262` 宿主对象；不支持写法的报错行号含拼进去的 harness

## ✍️ 约定


- 子模块私有 `mod`，crate 根 `pub use` 平铺导出，⛔ `pub mod`
- ⛔ `static` 与 `'static` 生命周期（枚举里的 `&'static str` 名字除外）；后台任务用 `Task<'t>` 借用调用方数据
- 字符串反序列化后立即转枚举，⛔ 拿字符串做判断
- 转换走 `From` / `TryFrom`
- 单元测试只钉确定的事实（每次只跑一个，`timeout 15`）
- 源码里不写注释：文档放在 README、指南和[本书](https://j8-mdbook.jabberwocky238.workers.dev/zh-CN/)里

## 📄 许可证

MIT.

<div align="center">

**🦀 同一门语言, 换一种写法.**

[⬆️ 回到顶部](#top) · [🇺🇸 English](README.md)

</div>
