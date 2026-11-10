# r8

用 Rust 重写 V8。Cargo workspace，多 crate 组织。

## 结构

| crate | std | 职责 |
|---|---|---|
| `r8frame` | `no_std` | 核心框架层：定义 trait（`Platform`、`Engine`、`Invoke`、`Intrinsics`）与通用结构体 / 枚举（`Isolate`、`Scope`、`Agent`、`Context`·`Realm`、`Global`、`Value`、`JsString`·`JsObject`、`Profiler`、`BinaryOp`、`Error`……），JS 运算语义挂在 `Agent` 上，实现归各层 crate |
| `r8builtins` | `no_std` | 实现 `Intrinsics`：往 `Realm` 装 Object / Function / Array / String / Number / Boolean / Math / Error 系列 / `console` / `parseInt` 等 |
| `r8platform` | std | 实现 `Platform`（`DefaultPlatform`）：后台任务跑在 moremath `ThreadMulti` 线程池上（起一次复用、任务可借用调用方数据），时钟用 `Instant` |
| `r8bytecode` | `no_std`（`compile` 要 std） | 累加器式寄存器字节码（`Insn` / `Function` / `Module`）；`compile` feature（缺省开）带 JS ⇒ 字节码编译器 `Module::try_from(&Source)`，用 oxc 解析 |
| `r8macros` | proc-macro | 过程宏：`enums!("x.json")` 从 JSON 生成枚举（定义、`Display`、按字段类型取 `&mut`），用法见 `crates/macros/USAGE.md` |
| `dune` | std | JS 运行时命令行（对标 node / deno）：`dune a.js` / `dune run a.js`（`-` 读标准输入）、`dune -e '<js>'`、`dune -p '<js>'` 打印结果、不带参数进 REPL（`.help` / `.exit`）；没接住的异常打 `Uncaught …` 退出码 1 |
| `r8debugger` | std | 调试命令行 `r8dbg`（clap）：`r8dbg asm <file.js \| ->` / `r8dbg asm -e '<js>'` 打印字节码反汇编（`-o` 写文件） |
| `r8executor` | `no_std`（`compile` 要 std） | 字节码执行器 `Executor`，**目前是最后一层**；`compile` feature 下实现 `Engine` |

所有 JS 都先编成字节码再执行，不设 AST 解释层。
依赖方向：各层只依赖 `r8frame`（`r8executor` 另依赖 `r8bytecode`），`r8frame` 不依赖任何层。

### 堆与句柄

堆用外部实现 [gc-arena](https://docs.rs/gc-arena)（增量标记清除，品牌生命周期，无 `'static`）：

- `Isolate::enter(|scope| …)` 进入一次，`'gc` 是这次进入的品牌；闭包期间不回收，返回后按欠账增量回收
- `Value<'gc>` 就是 V8 的 Local：出不了闭包，编译期保证
- `Global` 是持久句柄（gc-arena `DynamicRoot`），跨多次 `enter` 存活，drop 即放掉

```rust
use r8builtins::Builtins;
use r8executor::Executor;
use r8frame::{CreateParams, Isolate, Source};
use r8platform::DefaultPlatform;

let platform = DefaultPlatform::default();
let mut isolate = Isolate::new(&platform, CreateParams::default());
isolate.enter(|scope| {
    let cx = scope.context(&Builtins);
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

## 字节码

累加器式寄存器机（同 V8 Ignition）：结果都进隐式的 `acc`，二元运算 `acc = reg op acc`。

指令与 r8bytecode 的全部枚举（`Insn` / `Constant` / `FunctionKind` / `PropertyKind`）定义在 `crates/r8bytecode/insn.json`，由 `r8macros::enums!` 生成，⛔ 手写枚举；加指令只改 JSON（带 `Label` 字段的自动算跳转，进 `target_mut`）。

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

还不支持（报 `Error::Unsupported`，带 `文件:行:列`）：类的静态块 / 私有名 / 计算字段名、async 生成器 / `for await`。

已知偏差：一次 `enter` 内不回收（长循环会一直涨内存）。

## test262

`tests/test262` 是 tc39/test262 子模块（浅克隆，`git submodule update --init` 拉取），运行器 `tests/test262.rs` 挂在 dune 包下（`harness = false`，不另开 crate）：

```sh
cargo test -p dune --test test262 -- language/expressions/addition built-ins/Math   # 跑这些目录
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
