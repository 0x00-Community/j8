# j8jit

[English](README.md) · **简体中文**

[j8](https://github.com/0x00-Community/j8) 的 JIT（x86-64 Unix）。负责字节码 ⇒ 机器码与 JIT 调度本身：实现执行器的 `Tier`，
热函数与热循环降成 MLIR（`llvm` 方言，经 [mlirpass](https://crates.io/crates/mlirpass)），由 x86-64 后端（`x86_64.rs`）出机器码，
装进可执行内存，代替解释执行。

```text
j8bytecode::Function ─ Lower ─▶ MLIR（llvm 方言）─ Backend（x86-64）─▶ 机器码 + 要回填的地址
```

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

`Jit::from(n)` 设调用阈值（缺省 `Jit::THRESHOLD` = 100；循环要 `LOOP_FACTOR` 倍的迭代；0 表示每个函数第一次调用就编）。
`dune --jit` 和 `J8_TEST262_JIT=1` 打开它。

## 许可

MIT，随 [j8](https://github.com/0x00-Community/j8) 一起。
