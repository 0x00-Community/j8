# FINDING: 形参（非简单参数列表）缺 TDZ

**状态**：待确认 → 可提 issue / 可作为第一个 bug fix
**发现方式**：第一个练习里第 ⑤ 条用例（`function f(a = b, b) { return String(a) } f()`）与 V8 不一致
**影响面**：所有"形参列表含默认值 / 解构 / 剩余参数"，且某个形参的初值表达式引用了**排在自己后面**（尚未初始化）的形参

## 复现（j8 当前行为 vs 规范 / V8）

| # | 用例 | 规范 / V8 | j8（当前） |
|---|---|---|---|
| 1 | `function f(a = b, b) { return [a,b].join() } f()` | `ReferenceError: Cannot access 'b' before initialization` | `undefined`（不抛） |
| 2 | `function f(a = b, b) { return [a,b].join() } f(undefined, 2)` | 同上（**即使实参给了也要抛**） | `2`（不抛，读到了实参） |
| 3 | `function f(a = b, b) { return [a,b].join() } f(1, 2)` | `Ok("1,2")`（默认值不参与求值） | `Ok("1,2")` ✓ |
| 4 | `function f(a = a) { return String(a) } f()` | `ReferenceError: Cannot access 'a' before initialization` | `undefined`（不抛） |
| 5 | `function f(a = 1, b = a) { return [a,b].join() } f()` | `Ok("1,1")`（引用**前面**的参数是对的） | 待验证 |

第 3、5 条说明：不是"一律报错"，而是**按参数从左到右初始化**——第 i 个参数求默认值时，0..i-1 已初始化，i..n-1 仍在 TDZ 里（和实参有没有给无关）。

## 实测记录（练习场 `tests/explore.rs`，j8 release 构建）

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

## 机制定位

1. `crates/j8executor/src/lib.rs:526-541` `open_frame`：
   `regs.resize(base + usize::from(f.regs), Value::Undefined)` —— 没传的形参是 `undefined`，不是 TDZ 标记 `Uninit`。
2. `crates/j8executor/src/lib.rs:505-506`（`enter_func`）与 `:517-518`（`enter_regs`）：只把本次调用真正带来的实参拷进形参寄存器。
3. `crates/j8virtasm/src/compiler.rs:1112-1137` `prologue`：默认值**就地**在形参寄存器上求值
   （`Ldar(reg)` → `JumpIfNotUndefined` → 求初值 → `Star(reg)`），全程没有 `LdaHole`。
   ⇒ 求 `a` 的默认值时读 `b`，读到的是原始实参或 `undefined`，永远不会是 `Uninit`。
4. TDZ 机制本身是现成的，只是没用在形参上：
   - 指令：`crates/j8virtasm/insn.json:102-105`（`LdaHole` / `CheckInit` / `CheckReg` / `CheckEnv`）
   - 检查：`crates/j8executor/src/lib.rs:757-777`（`uninitialized()` → `ReferenceError: Cannot access '{name}' before initialization`，`lib.rs:777`）
   - 目前只用于 `let` / `const` / `class` / 派生类构造器的 `this`（`compiler.rs:1070-1075`）

## 修复骨架（两阶段 prologue）

只在 `!frame().simple_params` 时走新路径：

1. **存原始实参**：为每个形参分配一个临时寄存器，`Ldar(形参寄存器)` → `Star(临时寄存器)`。
2. **全部置为 TDZ**：对所有形参绑定 `LdaHole` → `Star(形参寄存器)`；若形参被闭包捕获（在 `Env` 里），它的 env 槽也要置 `Uninit`。
   ⚠️ `Env::with_len`（`crates/j8framework/src/object.rs`）现在用 `Value::Undefined` 填槽，形参槽需要 `Uninit`。
3. **按序绑定**：对第 i 个形参：`Ldar(临时寄存器)` → `JumpIfNotUndefined(skip)` → 求初值 → `Star(形参寄存器)` → `land(skip)`；然后照旧 `store(name, true)` / `bind_pattern`。

不受影响的部分（自查用）：
- `arguments` 的形参映射（`mapped_params`）只在 `simple_params && !strict` 时启用（`compiler.rs:785-791`、`:1059`），非简单参数列表根本不会走。
- 生成器 / async 的"参数错误同步抛"（`compiler.rs:1081-1089`）：prologue 仍在第一个 `Yield` 之前，顺序不变。

## 回归验证清单

```sh
# 1. 语义表里的用例（把上面 5 条加进去：1/2/4 进 thrown，3/5 进 ok）
cargo test --release -p j8executor javascript_semantics

# 2. 形参相关的 test262 目录（先 git submodule update --init）
J8_TEST262_VERBOSE=1 cargo test --release --test test262 -- \
  language/statements/function language/expressions/function language/expressions/arrow-function

# 3. 全量基线零回归
cargo test --release --test test262

# 4. 仓库要求
cargo clippy --all-targets
```

## 工程纪律

语义表 / test262 的期望值必须是**规范**，不是**当前实现**。
所以在修好之前，第 1/2/4 条**不能**以 `("…", "undefined")` 的形式进 `ok` 数组——那是把 bug 固化成回归网。
