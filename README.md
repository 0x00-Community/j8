# r8

用 Rust 重写 V8。Cargo workspace，多 crate 组织。

## 结构

| crate | std | 职责 |
|---|---|---|
| `r8frame` | `no_std` | 核心框架层：定义 trait（`Platform`、`Engine`、`Invoke`、`Intrinsics`）与通用结构体 / 枚举（`Isolate`、`Scope`、`Agent`、`Context`·`Realm`、`Global`、`Value`、`JsString`·`JsObject`、`Profiler`、`BinaryOp`、`Error`……），JS 运算语义挂在 `Agent` 上，实现归各层 crate |
| `r8`（仓库根 `src/`） | `no_std` | 聚合导出：`#[r8::class]` / `#[r8::object]` / `#[r8::function]`（宏在 r8macros，见 `crates/macros/USAGE.md`）与 r8binding 的 trait |
| `r8binding` | `no_std` | Rust ⇄ JS 绑定的运行时：`FromJs` / `IntoJs`、宿主对象、`Class` / `Object` |
| `r8builtins` | `no_std` | 实现 `Intrinsics`：往 `Realm` 装 Object / Function / Array / String / Number / Boolean / Math / Error 系列 / `console` / `parseInt` 等 |
| `r8platform` | std | 实现 `Platform`（`DefaultPlatform`）：后台任务与 GC 跑在 moremath `ThreadMulti` 线程池上（起一次复用、任务可借用调用方数据，经 `r8gc::Pool`），时钟用 `Instant` |
| `r8virtasm` | `no_std`（`compile` 要 std） | 虚拟指令集：累加器式寄存器字节码（`Insn` / `Function` / `Module`）；反汇编 `module.to_string()`、汇编 `text.parse::<Module>()`，互为逆；`compile` feature（缺省开）带 JS ⇒ 字节码编译器 `Module::try_from(&Source)`，用 oxc 解析 |
| `r8macros` | proc-macro | 过程宏：`enums!("x.json")` 从 JSON 生成枚举（定义、`Display`、按字段类型取 `&mut`），用法见 `crates/macros/USAGE.md` |
| `samples/dune` | std | 基于 r8 的 node 式二进制（`require` / `module` / `fs` / `path` / `process`，原生部分用 `#[r8::object]` 绑定）：`dune a.js` / `dune run a.js`（`-` 读标准输入）、`dune -e '<js>'`、`dune -p '<js>'` 打印结果、不带参数进 REPL（`.help` / `.exit`）；没接住的异常打 `Uncaught …` 退出码 1 |
| `samples/workerd` | std | 用 r8 加载 Cloudflare Worker（ES 模块 `export default { fetch }`）与 Durable Object：`workerd examples/counter.js --bind COUNTER=Counter --request "GET /c/inc?name=a"` 或 `--port 8787` 起 HTTP 服务；Fetch API 子集与 DO 在 JS 序言里，存储是 `#[r8::class]` 绑定的 Rust KV |
| `samples/benchv8` | std | 同一组 JS 基准在 r8 与 V8（rusty_v8 152，带 JIT / `--jitless`）上的耗时对比：`cd samples/benchv8 && cargo run --release`；依赖 V8 预编译库，自成 workspace（根 workspace 已 exclude） |
| `r8debugger` | std | 调试命令行 `r8dbg`（clap）：`r8dbg disasm <file.js \| ->` / `r8dbg disasm -e '<js>'` 打印反汇编清单；`r8dbg asm <file.r8s \| ->` 汇编清单、打印规范化后的清单（`-o` 写文件） |
| `r8executor` | `no_std`（`compile` 要 std） | 字节码执行器 `Executor`，**目前是最后一层**；`compile` feature 下实现 `Engine` |

所有 JS 都先编成字节码再执行，不设 AST 解释层。
依赖方向：各层只依赖 `r8frame`（`r8executor` 另依赖 `r8virtasm`），`r8frame` 不依赖任何层。

### 堆与句柄

堆用独立发布的 [r8gc](https://crates.io/crates/r8gc)（仓库 GitHub Jabberwocky238/r8gc、gitea jw238/r8gc）：分代（新生代碰撞分配、次回收复制提升）、
可移动（稀疏页搬空压缩）、增量标记，标记 / 次回收 / 清扫 / 压缩在平台的线程池上并行；品牌生命周期，无 `'static`。

- `Platform` 的父 trait 是 `r8gc::Pool`（`threads` / `run`）：后台任务与 GC 共用一个池（`DefaultPlatform` 的 moremath `ThreadMulti`）
- 回收只在 `enter` 之外发生，那时所有 `Gc` 都在根下面，所以对象能搬：⛔ 在不经 `Collect` 遍历的地方（`Static`、旁路缓存）存 `Gc` 或对象地址；
  按地址哈希的（Map / Set 的索引）在 `relocate` 里跟着改（见 `Collection`）
- 大容器在次回收里会整个重扫：`Collection` 记着「上次以来写过的条目从哪开始」，次回收只看那以后的（`Relocate::young_only`）

- `Isolate::enter(|scope| …)` 进入一次，`'gc` 是这次进入的品牌；闭包期间不回收，返回后按欠账增量回收
- `Engine::eval` 拿 `&mut Isolate` 自己分片进入：每片最多一万条指令，停下时执行状态挂到根上、`enter` 返回时回收一段再接着跑；执行环境用 `ContextRoot` 跨片保留（宿主函数里重入的 JS 在当前片里跑完）
- `Value<'gc>` 就是 V8 的 Local：出不了闭包，编译期保证
- `Global` 是持久句柄（r8gc `DynamicRoot`），跨多次 `enter` 存活，drop 即放掉

```rust
use r8builtins::Builtins;
use r8executor::Executor;
use r8frame::{CreateParams, Isolate, Source};
use r8platform::DefaultPlatform;

let platform = DefaultPlatform::default();
let mut isolate = Isolate::new(&platform, CreateParams::default());
let mut executor = Executor::default();
let cx = isolate.enter(|scope| scope.persist(scope.context(&Builtins)));
let result = isolate.run(&mut executor, &cx, &Source { text: "1 + 2 * 3", file: "a.js" })?;
isolate.enter(|scope| {
    let v = scope.local(&result);
});
```

验证 `no_std`：

```sh
cargo build --target riscv64gc-unknown-none-elf -p r8frame -p r8virtasm -p r8executor --no-default-features
```

## JIT（第一档）

`dune --jit a.js` 开 JIT；`R8_TEST262_JIT=1 cargo test -p dune --test test262` 开着 JIT（每个函数第一次调用、每个循环第一次往回跳就编）跑基线。

- `r8compiler`（`no_std`）：字节码函数 ⇒ MLIR（`llvm` 方言）⇒ mlirpass `target.x86_64` ⇒ x86-64 机器码（`Compiler` 建一次反复用，只装 builtin 与 `llvm` 方言）。
  机器码是 `extern "C" fn(regs, acc, start) -> u64`，直接读写本帧寄存器文件与累加器里的 `Value`（布局见 `r8frame::ValueTag`：16 字节、首字节标签、第 8 字节起载荷）；
  `start` 是 0 从函数开头进、`h + 1` 从循环头 `h` 进（OSR）；返回 0 是函数返回了，`pc + 1` 是停在第 pc 条指令之前（`r8executor::Exit`），解释器从那里接着跑
- 第一档收的指令：寄存器读写、数 / 布尔常量、四则（取余、乘方、位运算调运行时函数）与比较、`++` / `--`、跳转、死区检查、返回；
  操作数不是数等快路前提不成立就退回解释器；别的指令（调用、属性、闭包……）机器码停在它之前交给解释器
- 能整体在机器码里跑的函数进帧时就跑；别的函数只从「循环体里没有交给解释器的指令」的循环头切进去（OSR，从中途进的遇到 `return` 也交回解释器收尾）；
  两样都没有的函数不编
- `r8jit`（std，x86-64 Unix）：实现执行器的 `Tier`（`Executor::with_tier`）：调用次数过阈值（缺省 100）或循环圈数过 10 倍阈值就编、
  `mmap` 成可执行内存、填运行时函数地址；回答 `Tiered::Never` 的函数执行器记下、以后不再问
- 寄存器文件与累加器在机器码里是局部变量（每个寄存器一对 `llvm.alloca`：标签、载荷）：进来时从帧读一次、所有出口经同一个写回块写回帧；
  出机器码前跑 mlirpass `llvm.func(mem2reg, canonicalize, cse)`，局部变量提成 SSA，块之间按块参数传值
- 机器码后端是 mlirpass（crates.io，`=0.10.4`，`target.x86_64`，带线性扫描寄存器分配）；MLIR 用 `llvm` 方言的自定义写法（mlirformat 0.8.1 起真解析）
- 还没有：调用 / 属性 / 全局名等快路

## 字节码

累加器式寄存器机（同 V8 Ignition）：结果都进隐式的 `acc`，二元运算 `acc = reg op acc`。

指令与 r8virtasm 的全部枚举（`Insn` / `Constant` / `FunctionKind` / `PropertyKind`）定义在 `crates/r8virtasm/insn.json`，由 `r8macros::enums!` 生成，⛔ 手写枚举；加指令只改 JSON（带 `Label` 字段的自动算跳转，进 `target_mut`）。

### 汇编 / 反汇编

清单一个函数一段：函数头 `fn#i "名字" 种类 模式 params N length N regs N [generator] [async] [keeps_args]`，下面缩进的行依次是
`.mapped`（映射的 arguments 槽位，`-` 是不映射）、常量 `#k = number … | string "…" | bigint …`、
直接 eval 的作用域表 `scope#k = ["x" 3 const tdz] [ … ]`、指令 `@pc 助记符 操作数, …`；`;` 起到行尾是注释。编号必须按顺序写。

```text
fn#0 "main" normal sloppy params 0 length 0 regs 3
  #0 = string "x"
  @0    lda.const #0
  @1    return
```

一条指令的写法与 `Display` 同源：insn.json 里 `Insn` 开了 `asm`，`r8macros::enums!` 一并生成 `FromStr for Insn`，加指令不用改汇编器。

- 调用约定：`r0` 是 `this`，`r1` 是被调函数本身（具名函数表达式靠它递归），形参从 `r2` 起；`Call` 的被调者、`this`、实参排在连续寄存器
- 作用域：编译前预扫一遍（`scope.rs` 的 `Resolver`），找出被内层函数捕获的绑定放进堆上的 `Env` 链（`PushEnv` / `PopEnv` / `LdaEnv depth.slot`），其余放寄存器；`for (let …)` 每轮 `CloneEnv`，闭包各拿各的那一轮
- 脚本顶层的 `var` / 函数 / `let` / `const` 是全局对象的属性（`LdaGlobal` / `StaGlobal`）
- 执行器用显式帧栈（不靠宿主递归），调用深度上限 10000（含宿主函数里重入的 JS），超了抛 RangeError
- 异常：每帧一个动态处理器栈（`PushHandler` / `PopHandler`），抛出时弹本帧处理器、没有就展开到调用者；`finally` 用（完成类型, 值）寄存器对，穿过它的 `return` / `break` / `continue` 先绕进去再接着走

`r8executor` 里的 `javascript_semantics` 测试是一张逐条确定的 JS 语义表。

## 现状

支持：

- 字面量（数 / 字符串 / 布尔 / `null` / 模板字符串）、对象字面量（含方法简写、计算键）、数组字面量
- 全部算术、比较（`==` / `===` / 字符串按 UTF-16 比）、位运算、`**`、`typeof` / `void` / `!` / `~`、`&&` / `||` / `??`、`?:`、逗号
- 赋值与复合赋值（含 `||=` / `&&=` / `??=`）、`++` / `--`，目标可以是名字或属性
- `var` / `let` / `const`（块级作用域、对 `const` 赋值抛 TypeError）、函数声明提升
- `if` / `while` / `do-while` / `for` / `switch` / 带标签的 `break` / `continue` / `return` / `throw`
- 函数声明 / 表达式 / 箭头函数（词法 `this`）、闭包、方法调用的 `this`
- 属性读写（数组下标与 `length`、字符串下标与 `length`、原型链读）
- `try` / `catch` / `finally`（`finally` 里的 `return` / `break` / `continue` 与穿过 `finally` 的跳转都按规范）、Error 对象（`Error` / `TypeError` / `ReferenceError` / `RangeError` / `SyntaxError`）
- 内建对象（r8builtins）与宿主函数、`console.log`（经 `Platform::print`）、ToPrimitive（`valueOf` / `toString`）
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

还不支持（报 `Error::Unsupported`，带 `文件:行:列`）：模块的顶层 await、形参表达式里 eval 的 `arguments` 冲突规则。test262 基线 42430 条，补齐计划见 PLAN.md。

已知偏差：宿主函数里重入的 JS（如 `map` 的回调）在当前片里跑完，期间不回收（`Function.prototype.call` / `apply` 由执行器直接展开成普通调用，不算重入）。

## test262

`tests/test262` 是 tc39/test262 子模块（浅克隆，`git submodule update --init` 拉取），运行器 `tests/test262.rs` 挂在 dune 包下（`harness = false`，不另开 crate）：

```sh
cargo test -p dune --test test262 -- language/expressions/addition built-ins/Math   # 跑这些目录
R8_TEST262_JIT=1 cargo test -p dune --test test262                                   # 开 JIT（每个函数第一次调用就编）对拍
R8_TEST262_VERBOSE=1 cargo test -p dune --test test262 -- <路径>                    # 逐条列出没过的原因
R8_TEST262_UPDATE=1 cargo test -p dune --test test262 -- <路径>                     # 结果写回基线
cargo test -p dune --test test262                                                   # 只跑基线里的用例，掉了就失败
```

- 每个用例一个子进程、10 s 超时；结果分 pass / fail / unsupported（r8 报 `Error::Unsupported`）/ skip / timeout / crash，按目录汇总
- 基线 `tests/test262.passing`：过了的用例一行一个；不带参数时只跑它做回归检查
- 只跑非严格模式（`onlyStrict` 跑严格）；`module` 跳过；没有 `$262` 宿主对象；不支持写法的报错行号含拼进去的 harness

## 约定

- 尽可能 `no_std`（`core` + `alloc`），要 std 的部分放在 feature 后面

- 子模块私有 `mod`，crate 根 `pub use` 平铺导出，⛔ `pub mod`
- ⛔ `static` 与 `'static` 生命周期（枚举里的 `&'static str` 名字除外）；后台任务用 `Task<'t>` 借用调用方数据
- 字符串反序列化后立即转枚举，⛔ 拿字符串做判断
- 转换走 `From` / `TryFrom`
- 单元测试只钉确定的事实（每次只跑一个，`timeout 15`）
