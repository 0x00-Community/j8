# j8jit

[English](README.md) · **简体中文**

[j8](https://github.com/0x00-Community/j8) 的 JIT（x86-64 Unix 和 Windows、AArch64 Linux）。负责字节码 ⇒ 机器码与 JIT 调度本身：实现执行器的 `Tier`，
热函数与热循环先建成推测式 SSA 图（`build.rs`），再经下面两条路之一变成机器码，装进可执行内存，代替解释执行。

```text
j8bytecode::Function ─ build ─▶ 图 ─┬─ codegen.rs ─────────────────────▶ 机器码（缺省；x86-64、AArch64）
                                    └─ stitch.rs：拷贝片段、填空位 ─▶ 机器码（J8_JIT_MLIR=1；x86-64、AArch64）
模板（图里每种指令一个）─ mlirpass 管线 + x86_64.rs / aarch64.rs，每个进程一次 ─▶ 片段
```

拼接这条路就是 [mlirpass](https://crates.io/crates/mlirpass) 的快路径（`Stencils`）：每个模板在进程第一次用 JIT 时走一遍管线
和 x86-64 或 AArch64 后端；之后编一个函数只是拷贝片段、填空位（帧槽、常量、跳到 deopt 出口），不再为每个函数构造 MLIR。

```text
调用 / 迭代没到阈值      ⇒ 计数，照常解释
到阈值                   ⇒ 编译（失败记下，不再重试）
进帧                     ⇒ 在本帧的寄存器上跑机器码
循环回跳                 ⇒ 在该循环头切进机器码（OSR）
停在某条指令之前         ⇒ 解释器从那里接着跑
```

## 用法

```rust
use std::rc::Rc;
let executor = j8executor::Executor::default().with_tier(Rc::new(j8jit::Jit::default()));
```

`Jit::from(n)` 设调用阈值（缺省 `Jit::THRESHOLD` = 64；0 表示每个函数第一次调用就编）。
`dune --jit` 和 `J8_TEST262_JIT=1` 打开它。

## WebAssembly

`WasmJit` 是 WebAssembly 的层（`WasmStore::with_tier`；JS API 用 `WebAssembly::from(builtins).with_tier(Arc::new(WasmJit::default()))`）。
走同一条模板快路径：每类 wasm 字节码一个模板，进程里经 mlirpass 编一次；函数被调到阈值（`WasmJit::THRESHOLD` = 16）后按模板拼成
机器码，寄存器放在原生栈帧里。整数和 f64 运算、比较、常量、分支、`br_table`、`select`、线性内存读写直接是机器码；其他标量运算
调一个数值辅助函数；直接调用直接进被调函数的机器码；其余（全局、表、GC、间接调用、抛异常）在同一组寄存器上让解释器单步执行一条。
碰 `v128`、带异常处理或用 `br_on_cast` 的函数仍由解释器执行。

## 许可

MIT，随 [j8](https://github.com/0x00-Community/j8) 一起。
