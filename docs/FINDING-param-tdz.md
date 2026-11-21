# FINDING: 形参（非简单参数列表）缺 TDZ

**状态**：已确认为 bug（j8 侧），可作为第一个 bug fix
**发现方式**：第一个练习里第 ⑤ 条用例（`function f(a = b, b) { return String(a) } f()`）与 V8 不一致
**影响面**：所有"形参列表含默认值 / 解构 / 剩余参数"，且某个形参的初值表达式引用了**排在自己后面**（尚未初始化）的形参

> **行号说明**：下文行号基于本仓库 `4dbea38`（`crates/j8executor/src/lib.rs` 共 2378 行）。
> 行号会随提交漂移，所以每个位置都同时给了**符号名**，并附「重新定位」的命令（见第 5 节）。

## 1. 复现（j8 当前行为 vs 规范 / V8）

| # | 用例 | 规范 / V8 | j8（当前） |
|---|---|---|---|
| 1 | `function f(a = b, b) { return [a,b].join() } f()` | `ReferenceError: Cannot access 'b' before initialization` | `undefined`（不抛） |
| 2 | 同上，`f(undefined, 2)` | 同上（**即使实参给了也要抛**） | `2`（不抛，读到了实参） |
| 3 | 同上，`f(1, 2)` | `Ok("1,2")`（默认值不参与求值） | `Ok("1,2")` ✓ |
| 4 | `function f(a = a) { return String(a) } f()` | `ReferenceError: Cannot access 'a' before initialization` | `undefined`（不抛） |
| 5 | `function f(a = 1, b = a) { return [a,b].join() } f()` | `Ok("1,1")`（引用**前面**的参数是对的） | `Ok("1,1")` ✓ |

第 3、5 条说明：不是"一律报错"，而是**按参数从左到右初始化**——第 i 个参数求默认值时，0..i-1 已初始化，
i..n-1 仍在 TDZ 里（和实参有没有给无关）。

## 2. 实测记录（练习场 `tests/explore.rs`，j8 release 构建）

用例都写成 `function f(...) { return String(a) }` 的形式：

| 调用 | j8 实测 | 规范 / V8 |
|---|---|---|
| `f()` | `Ok(undefined)` | ReferenceError ✗ |
| `f(undefined, 2)` | `Ok(2)`（读到了实参，不抛） | ReferenceError ✗ |
| `f(1, 2)` | `Ok(1)` | `Ok("1")` ✓ |
| `f(a = a)`，`f()` | `Ok(undefined)` | ReferenceError ✗ |
| `f(a = 1, b = a)`，`f()` | `Ok(1)` | `Ok("1")` ✓ |

结论：**引用前面已初始化的形参没问题，引用后面（含自己）的形参一律不抛** —— TDZ 在形参这条路上完全不存在，
不是"只在没传实参时才错"。

`f(undefined, 2)` 得到 `2`，正好证实机制定位第 3 条：`a` 的默认值读 `b` 时，读到的是 `b` 寄存器里那份**原始实参**，
TDZ 从未被建立。对修复的含义：**把形参寄存器置成 `Uninit` 之前，必须先把实参搬到临时寄存器**，
否则修完这个用例会变成"传进来的值也丢了"。这条用例因此必须进验收清单。

## 3. 机制定位

| 符号 | 位置（`4dbea38`） | 作用 / 问题 |
|---|---|---|
| `Executor::open_frame` | `crates/j8executor/src/lib.rs:526` | 新帧的寄存器用 `Value::Undefined` 铺满 ⇒ 没传的形参是 `undefined`，不是 TDZ 标记 `Uninit` |
| `Executor::enter_func` / `enter_regs` | 同文件 `:503` / `:515` | 只把本次调用真正带来的实参拷进形参寄存器 |
| `Compiler::prologue` | `crates/j8virtasm/src/compiler.rs:1112` | 默认值**就地**在形参寄存器上求值（`Ldar(reg)` → `JumpIfNotUndefined` → 求初值 → `Star(reg)`），全程没有 `LdaHole` |
| `Compiler::bind_pattern` | 同文件 `:1150` | 形参/解构的绑定落位；`init: bool` 表示"这是初始化" |
| `simple_params` 判定 | 同文件 `:1059` | 参数列表是否"简单"（无默认值 / 解构 / rest） |
| 派生类 `this` 的未初始化 | 同文件 `:1073` 附近 | TDZ 机制目前**唯一**用在形参相关位置的样例，可照着抄 |
| `LdaHole` / `CheckInit` / `CheckReg` / `CheckEnv` | `crates/j8virtasm/insn.json:102-105` | TDZ 的指令级机制（写入哨兵 / 读取时检查） |
| `Insn::LdaHole` 的执行 | `crates/j8executor/src/lib.rs:1122` | `*acc = Value::Uninit` |
| `Executor::uninitialized` | 同文件 `:776`（抛在 `:777`） | 生成 `ReferenceError: Cannot access '{name}' before initialization` |
| `Env::with_len` | `crates/j8framework/src/object.rs:72` | 环境槽用 `Value::Undefined` 填 ⇒ 被闭包捕获的形参也没有 TDZ |

也就是说：TDZ 的零件齐全（哨兵值 + 检查指令 + 报错函数），唯独**形参这条路没有接上**——
`prologue` 直接拿实参寄存器当"已初始化"的绑定用。

## 4. 修复骨架（两阶段 prologue）

只在 `!frame().simple_params` 时走新路径：

1. **存原始实参**：为每个形参分配临时寄存器，`Ldar(形参寄存器)` → `Star(临时寄存器)`。
   （不做这步就会丢实参：见第 2 节的 `f(undefined, 2)` → `2`。）
2. **全部置为 TDZ**：对所有形参绑定 `LdaHole` → `Star(形参寄存器)`；被闭包捕获的形参，
   它在 `Env` 里的槽也要置 `Uninit` ⇒ `Env::with_len` 需要能区分"形参槽"（或者由 `prologue` 显式写 `Uninit`）。
3. **按序绑定**：对第 i 个形参：`Ldar(临时寄存器)` → `JumpIfNotUndefined(skip)` → 求初值 →
   `Star(形参寄存器)` → `land(skip)`；然后照旧 `store(name, true)` / `bind_pattern(..., init: true)`。

## 5. 行号怎么重新定位（行号一定会漂）

```sh
cd ~/workspace/j8

# 语义表与四个插入锚点
grep -n 'fn javascript_semantics' crates/j8executor/src/lib.rs
grep -n 'let ok = \[\|let thrown = \[\|let printed = \[' crates/j8executor/src/lib.rs
grep -n '"\[3, 1, 2\]\.sort\|\.map(x => x \* 2)\.filter\|const s = Symbol\|{ x; let x = 1 }' crates/j8executor/src/lib.rs

# 执行器
grep -n 'fn open_frame\|fn enter_func\|fn enter_regs\|fn uninitialized\|Insn::LdaHole =>' crates/j8executor/src/lib.rs

# 编译器
grep -n 'fn prologue\|simple_params =\|fn bind_pattern' crates/j8virtasm/src/compiler.rs

# 指令与框架
grep -n '"LdaHole"\|"CheckInit"\|"CheckReg"\|"CheckEnv"' crates/j8virtasm/insn.json
grep -n 'fn with_len' crates/j8framework/src/object.rs
```

两个已知的偏移量：

- **若已经加上 PR #1 的 4 条用例**（3 处 `注释 + 用例` + 1 条只加用例 = 共 7 行），则
  `let thrown = [` 由 2298 → **2304**，thrown 里的锚点由 2320 → **2326**，`let printed = [` 由 2351 → **2357**，
  文件总行数 2378 → **2385**；`fn javascript_semantics`（2077）与 `ok` 前面的锚点不变。
- **若 `git pull` 拉到了新提交**：以上数字全部作废，用上面的 `grep` 重新取。

## 6. 不受影响的部分（自查用）

- `arguments` 的形参映射（`mapped_params`）只在 `simple_params && !strict` 时启用
  （`crates/j8virtasm/src/compiler.rs:785-791` 与 `:1059`），非简单参数列表根本不会走。
- 生成器 / async 的"参数错误同步抛"（同文件 `:1081-1089`）：`prologue` 仍在第一个 `Yield` 之前，顺序不变。

## 7. 回归验证清单

```sh
# 1. 语义表：把第 1 节的 1/2/4 条进 thrown，3/5 条进 ok
cargo test --release -p j8executor javascript_semantics

# 2. 形参相关的 test262 目录（先 git submodule update --init）
J8_TEST262_VERBOSE=1 cargo test --release --test test262 -- \
  language/statements/function language/expressions/function language/expressions/arrow-function

# 3. 全量基线零回归
cargo test --release --test test262

# 4. 仓库要求
cargo clippy --all-targets
```

## 8. 工程纪律

语义表 / test262 的期望值必须是**规范**，不是**当前实现**。
所以在修好之前，第 1/2/4 条**不能**以 `("…", "undefined")` 的形式进 `ok` 数组——那是把 bug 固化成回归网。
