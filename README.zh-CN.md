<a id="top"></a>
<div align="center">

# j8

### 用 Rust 重写 V8.
**字节码解释器 + 经 MLIR 的 JIT + 并行回收的堆 ([paragc](https://crates.io/crates/paragc)).**

[![Rust](https://img.shields.io/badge/Rust-100%25-orange?style=for-the-badge&logo=rust)](https://www.rust-lang.org)
[![test262](https://img.shields.io/badge/test262-42%2C443%20%E9%80%9A%E8%BF%87-success?style=for-the-badge)](#-test262)
[![License](https://img.shields.io/badge/License-MIT-brightgreen?style=for-the-badge)](#-许可证)

[![ECMAScript](https://img.shields.io/badge/ECMAScript-%E6%A0%B8%E5%BF%83%E8%AF%AD%E8%A8%80-F7DF1E?style=flat-square&logo=javascript&logoColor=black)](#-现状)
[![JIT](https://img.shields.io/badge/JIT-x86--64%20via%20MLIR-262D3A?style=flat-square&logo=llvm)](#-jit第一档)
[![GC](https://img.shields.io/badge/GC-paragc%20%C2%B7%20%E5%B9%B6%E8%A1%8C%20%C2%B7%20%E6%97%A0%E9%94%81-8A2BE2?style=flat-square)](#-堆与句柄)

[🇺🇸 English](README.md) · **🇨🇳 简体中文**

</div>

---

## ⚡ j8 是什么

j8 是**用 Rust 从头写的 JavaScript 引擎**, 照着 V8 的分层来: 所有 JS 先编成累加器式字节码 (同 V8 Ignition),
由执行器解释, 热函数和热循环交给 JIT 出 x86-64 机器码. 堆是 [paragc](https://crates.io/crates/paragc), 外面包一层 `j8framework` 的 GC 层.

🎯 **主线:** test262 通过率. 核心 ECMAScript 按 [PLAN.md](PLAN.md) 逐阶段补齐, 每一步跑基线零回归.

🧩 **可嵌入:** `#[j8::class]` / `#[j8::object]` / `#[j8::function]` 一行把 Rust 类型绑成 JS; 自带两个样例: 分 Web 标准层
与 Node 兼容层的运行时 `dune`, 以及 Cloudflare Worker 式运行时 `workerd`. 照样搭自己的宿主: 先看 [EMBEDDING.md](EMBEDDING.md)（各部分怎么拼, 英文）,
宏的逐项说明在 [crates/macros/USAGE.md](crates/macros/USAGE.md).

## 🔥 凭什么不一样

| | |
| --- | --- |
| 🦀 **全 Rust** | 解析用 oxc, 其余从字节码到 GC 都是自己写的. 没有 C++ 引擎在下面. |
| 🧱 **字节码先行** | 不设 AST 解释层: 所有 JS (包括 `eval`、`Function`) 都先编成字节码. 全部字节码类型由 `insn.json` 生成; 字节码没有文本形式, 只有二进制形式. |
| 🧬 **JIT 走 MLIR** | 字节码 ⇒ MLIR `llvm` 方言 ⇒ [mlirpass](https://crates.io/crates/mlirpass) 的 x86-64 后端 (线性扫描寄存器分配), 支持 OSR. |
| ♻️ **paragc 堆** | Immix 式块 / 行堆: 年轻回收 + 老对象引用计数、稀疏块疏散, 标记与清扫并行, 无锁; `'gc` 品牌生命周期, 编译期保证句柄不逃逸. |
| 🚫 **没有 `'static`** | 不用全局状态; 后台任务借用调用方数据, 多个 Isolate 共用一个线程池. |

## 🏆 数字

| ✔️ 项目 | 结果 |
| --- | --- |
| 📜 test262 基线 | **42,443** 条通过 (非严格 + `onlyStrict`; 不含 Temporal / intl402 等, 见 PLAN.md), 每次提交零回归 |
| 🧮 JIT | `J8_TEST262_JIT=1` 开着 JIT (每个函数第一次调用就编) 跑同一份基线 |
| ⏱️ 对 V8 jitless | 11 个基准几何平均慢 **3.7×**, 校验和三种引擎全一致 |
| 🚄 对 V8 (带 JIT) | 几何平均慢 **9.4×**; 差距主要在调用和分配, 详见 [benchmark.md](samples/benchv8/benchmark.md) |

## 📦 安装

```sh
cargo add j8                                    # 缺省 feature `runtime`: 执行器 + 内建对象 + 标准平台
cargo add j8 --features jit                     # 再加 x86-64 JIT (x86-64 Unix)
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
| `j8framework` | std | 核心框架层：定义 trait（`Platform`、`Engine`、`Invoke`、`Intrinsics`）与通用结构体 / 枚举（`Isolate`、`Scope`、`Agent`、`Context`·`Realm`、`Global`、`Value`、`JsString`·`JsObject`、`Profiler`、`BinaryOp`、`Error`……），JS 运算语义挂在 `Agent` 上，实现归各层 crate。`EventLoop` 是一个 JS 事件循环（一个 isolate、它的引擎与一个上下文；可续跑的 `Engine::start` / `resume`、宿主异步活）；`EventLoopSet` 把许多事件循环钉在少数 loop 线程上跑，带负载均衡（放置、工作窃取、`rebalance`、`split`）与统一配额 |
| `j8`（仓库根 `src/`） | std | 聚合导出：`#[j8::class]` / `#[j8::object]` / `#[j8::function]`（宏在 j8macros，见 `crates/macros/USAGE.md`）与 j8binding 的 trait；其余 crate 按 feature 导出（缺省 `runtime` = `j8::executor` + `j8::builtins` + `j8::platform`，另有 `bytecode` / `taskqueue` / `lockfree` / `jit` / `full`），每个子 crate 也单独发在 crates.io |
| `j8binding` | std | Rust ⇄ JS 绑定的运行时：`FromJs` / `IntoJs`、宿主对象、`Class` / `Object` |
| `j8builtins` | std | 实现 `Intrinsics`：往 `Realm` 装 Object / Function / Array / String / Number / Boolean / Math / Error 系列 / `console` / `parseInt` 等 |
| `j8platform` | std | 实现 `Platform`（`DefaultPlatform`）：后台任务跑在 moremath `ThreadMulti` 线程池上（起一次复用、任务可借用调用方数据，经 `j8framework::Pool`），时钟用 `Instant` |
| `j8bytecode` | std | 虚拟指令集，依赖根（不依赖其他 j8 crate）：累加器式寄存器字节码（`Insn` / `Function` / `Module` …），全部类型由 `insn.json` 生成；没有文本形式，只有二进制形式 `Vec::<u8>::from(&module)` / `Module::try_from(bytes)` |
| `j8compiler` | std | JS ⇒ 字节码（`Module` 实现 `j8compiler::Compile`：脚本、模块、`eval` / `Function`），用 oxc 解析 |
| `j8taskqueue` | `no_std` | 任务队列：`ITask`（`recursable` / `depend` / `exec`）与 `ITaskPoll`（`add` / `run`）；无锁数据流调度（手写 MPMC 就绪队列、原子依赖计数），依赖做完的任务立刻在宿主给的 `Pool` 上并行跑（或就在调用线程上跑）、没有轮次屏障；`TaskId::family` 并查集找爹、`cancel` 整家取消；配额按字节码条数（`Quota` / `Flow::Yield`、每次 run 的预算）；取不到活的线程直接离开、不自旋。每个 `j8framework::EventLoop` 就是一个 `Queue`：脚本、兑现、宿主 future 的 poll，脚本每让出一次就检查一次队列 |
| `j8macros` | proc-macro | 过程宏：`enums!("x.json")` 从 JSON 生成枚举（定义、`Display`、按字段类型取 `&mut`）、堆类型的 `#[derive(Collect)]`（paragc `Trace`）、绑定宏，用法见 `crates/macros/USAGE.md` |
| `dune` | std | 对标 Node 的运行时，分两层：`src/web/`（Web 标准接口：`console`、定时器、`URL`、`TextEncoder`、`EventTarget` / `AbortController`、`fetch`、`structuredClone`、`crypto`……）和叠在上面的 `src/node/`（`process`、`require` / ES 模块、`Buffer`、`fs`、`path`、`os`、`events`、`util`、`assert`、`child_process`……）；碰机器的部分是 `#[j8::object]` / `#[j8::class]` 绑定的 Rust，接口本身是 JS prelude；`dune a.js`、`dune --web a.mjs`、`dune -e` / `-p`、REPL，详见其 README |
| `samples/workerd` | std | 用 j8 加载 Cloudflare Worker（ES 模块 `export default { fetch }`）与 Durable Object：`workerd examples/counter.js --bind COUNTER=Counter --request "GET /c/inc?name=a"` 或 `--port 8787` 起 HTTP 服务；Fetch API 子集与 DO 在 JS 序言里，存储是 `#[j8::class]` 绑定的 Rust KV |
| `samples/benchv8` | std | 同一组 JS 基准在 j8 与 V8（rusty_v8 152，带 JIT / `--jitless`）上的耗时对比：`cd samples/benchv8 && cargo run --release`（会下载 V8 预编译库） |
| `samples/j8bcdbg` | std | 字节码调试命令行（clap）：`j8bcdbg compile <file.js \| ->` / `j8bcdbg compile -e '<js>'` 按函数列出字节码，格式照 V8 的 `--print-bytecode`（`-o` 写文件） |
| `j8executor` | std | 字节码执行器 `Executor`，**目前是最后一层**；实现 `Engine`，固定带 JS ⇒ 字节码编译器（脚本、模块、`eval` / `Function` 都先编成字节码） |

所有 JS 都先编成字节码再执行，不设 AST 解释层。
依赖方向：各层只依赖 `j8framework`（`j8executor` 另依赖 `j8compiler`；`j8bytecode` 是根，`j8framework` 与 `j8compiler` 依赖它），`j8framework` 不依赖任何层，只依赖基础件 `j8lockfree` 与 `j8taskqueue`。

### 🧹 堆与句柄

堆是 [paragc](https://crates.io/crates/paragc)，只用它的公开接口；`j8framework` 的 GC 层在上面补 JS 引擎要的东西：`GcCell`
（无锁的 `RefCell`，数据放在 paragc `Slot` 指着的快照里：上次回收后新分配的快照原地改，旧的先挪进一份新快照再改，写屏障因此看得见）、
`ValueCell`（一个 NaN-boxing 的 `Value`：原子字 + `Slot`）、`Cow`、`GcQueue`、`Static`，以及 `#[derive(Collect)]`（在 j8macros；
`#[collect(require_static)]` 跳过一个字段或整个类型）。品牌生命周期，无 `'static`。

- 回收只在 `enter` 之外、所有 `Gc` 都挂在根上时发生，对象会被搬：⛔ 把 `Gc` 或对象地址存进 `Trace` 不遍历的地方（`Static`、旁路缓存）；
  按地址哈希的东西（Map / Set 的索引）在 `relocate` 里更新（见 `Collection`）
- ⛔ 给还没进堆的值写 `Slot` / `ValueCell` / `Cow`（写屏障记的是槽地址）：先分配进堆再填（见 `Scope::context`）
- 堆只做停顿式回收（`collect_if_needed` / `collect`，内部并行）；`GcCell` 原地改年轻快照不经写屏障，所以 ⛔ `mark_concurrently`
- 堆里的类型都是 `Send + Sync`（paragc 的规矩）：宿主数据是 j8lockfree `AtomicRefCell` 里的 `Box<dyn Any + Send + Sync>`，
  Shape 与 rope 用 `Arc`
- 堆有自己的回收线程（数目同平台线程池）；`Platform` 的父 trait `Pool` 给并行内建函数用

- `Isolate::enter(|scope| …)` 进入一次，`'gc` 是这次进入的品牌；闭包期间不回收，返回后分配够多就回收（通常是年轻回收，每隔几轮一次完整回收）
- `Engine::eval` 拿 `&mut Isolate` 自己分片进入：每片最多一万条指令，停下时执行状态挂到根上、`enter` 返回时回收一段再接着跑；执行环境用 `ContextRoot` 跨片保留（宿主函数里重入的 JS 在当前片里跑完）
- `Value<'gc>` 就是 V8 的 Local：出不了闭包，编译期保证
- `Global` 是持久句柄（paragc `Root`），跨多次 `enter` 存活，drop 即放掉

```rust
use j8builtins::Builtins;
use j8executor::Executor;
use j8framework::{CreateParams, Isolate, Source};
use j8platform::DefaultPlatform;

let platform = DefaultPlatform::default();
let mut isolate = Isolate::new(&platform, CreateParams::default());
let mut executor = Executor::default();
let cx = isolate.enter(|scope| scope.persist(scope.context(&Builtins)));
let result = isolate.run(&mut executor, &cx, &Source { text: "1 + 2 * 3", file: "a.js" })?;
isolate.enter(|scope| {
    let v = scope.local(&result);
});
```

## 🧬 JIT（第一档）

`dune --jit a.js` 开 JIT；`J8_TEST262_JIT=1 cargo test --release --test test262` 开着 JIT（每个函数第一次调用、每个循环第一次往回跳就编）跑基线。

- `j8jit` 编译部分：字节码函数 ⇒ MLIR（`llvm` 方言）⇒ mlirpass `target.x86_64` ⇒ x86-64 机器码（`Compiler` 建一次反复用，只装 builtin 与 `llvm` 方言）。
  机器码是 `extern "C" fn(regs, acc, start) -> u64`，直接读写本帧寄存器文件与累加器里的 `Value`（布局见 `j8framework::ValueTag`：16 字节、首字节标签、第 8 字节起载荷）；
  `start` 是 0 从函数开头进、`h + 1` 从循环头 `h` 进（OSR）；返回 0 是函数返回了，`pc + 1` 是停在第 pc 条指令之前（`j8executor::Exit`），解释器从那里接着跑
- 第一档收的指令：寄存器读写、数 / 布尔常量、四则（取余、乘方、位运算调运行时函数）与比较、`++` / `--`、跳转、死区检查、返回；
  操作数不是数等快路前提不成立就退回解释器；别的指令（调用、属性、闭包……）机器码停在它之前交给解释器
- 能整体在机器码里跑的函数进帧时就跑；别的函数只从「循环体里没有交给解释器的指令」的循环头切进去（OSR，从中途进的遇到 `return` 也交回解释器收尾）；
  两样都没有的函数不编
- `j8jit`（std，x86-64 Unix）：实现执行器的 `Tier`（`Executor::with_tier`）：调用次数过阈值（缺省 100）或循环圈数过 10 倍阈值就编、
  `mmap` 成可执行内存、填运行时函数地址；回答 `Tiered::Never` 的函数执行器记下、以后不再问
- 寄存器文件与累加器在机器码里是局部变量（每个寄存器一对 `llvm.alloca`：标签、载荷）：进来时从帧读一次、所有出口经同一个写回块写回帧；
  出机器码前跑 mlirpass `llvm.func(mem2reg, canonicalize, cse)`，局部变量提成 SSA，块之间按块参数传值
- 机器码后端是 mlirpass（crates.io，`=0.10.4`，`target.x86_64`，带线性扫描寄存器分配）；MLIR 用 `llvm` 方言的自定义写法（mlirformat 0.8.1 起真解析）
- 还没有：调用 / 属性 / 全局名等快路

## 🔣 字节码

累加器式寄存器机（同 V8 Ignition）：结果都进隐式的 `acc`，二元运算 `acc = reg op acc`。

j8bytecode 的全部类型（指令、操作符、常量、`Function` / `Module` 等结构体）定义在 `crates/j8bytecode/insn.json`，由 `j8macros::enums!` 生成，⛔ 手写类型定义；加指令只改 JSON（带 `Label` 字段的自动算跳转，进 `target_mut`）。

### 二进制形式

字节码没有文本形式，存储与分发只用 insn.json（开 `binary`）生成的二进制形式：魔数 ` j8b`、insn.json 指纹（只有同一套指令集能读回）、模块本体，枚举是一字节标签加字段。

- 调用约定：`r0` 是 `this`，`r1` 是被调函数本身（具名函数表达式靠它递归），形参从 `r2` 起；`Call` 的被调者、`this`、实参排在连续寄存器
- 作用域：编译前预扫一遍（`scope.rs` 的 `Resolver`），找出被内层函数捕获的绑定放进堆上的 `Env` 链（`PushEnv` / `PopEnv` / `LdaEnv depth.slot`），其余放寄存器；`for (let …)` 每轮 `CloneEnv`，闭包各拿各的那一轮
- 脚本顶层的 `var` / 函数 / `let` / `const` 是全局对象的属性（`LdaGlobal` / `StaGlobal`）
- 编译完每个函数跑一遍窥孔融合（`fuse.rs` 的 `Function::fuse`）：`lda.smi n; add r` ⇒ `add.smi r, n`、`ldar y; add x` ⇒ `add.reg x, y`（比较同理）、`ldar a; star b` ⇒ `mov b, a`、`ldar r; inc; star r` ⇒ `inc.reg r`、`star r; ldar r` ⇒ `star r`、紧接着的 `mov b, a; mov a, b` 丢掉第二条；
  组里第一条之后有跳转落点就不融合，跳转目标按新下标改写。融合指令的语义就是原来几条依次执行（`Insn::unfused` 拆回），JIT 照拆回的逐条下降
- 执行器用显式帧栈（不靠宿主递归），调用深度上限 10000（含宿主函数里重入的 JS），超了抛 RangeError
- 异常：每帧一个动态处理器栈（`PushHandler` / `PopHandler`），抛出时弹本帧处理器、没有就展开到调用者；`finally` 用（完成类型, 值）寄存器对，穿过它的 `return` / `break` / `continue` 先绕进去再接着走

`j8executor` 里的 `javascript_semantics` 测试是一张逐条确定的 JS 语义表。

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

## 📄 许可证

MIT.

<div align="center">

**🦀 同一门语言, 换一种写法.**

[⬆️ 回到顶部](#top) · [🇺🇸 English](README.md)

</div>
