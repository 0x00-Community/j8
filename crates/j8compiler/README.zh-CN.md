# j8compiler

[English](README.md) · **简体中文**

[j8](https://github.com/0x00-Community/j8) JIT 用的字节码编译器：一个字节码函数 ⇒ MLIR（`llvm` 方言，经
[mlirpass](https://crates.io/crates/mlirpass)）⇒ 经 `Backend` 出机器码（x86-64 后端在 `j8jit`）。

```text
j8virtasm::Function ─ Lower ─▶ MLIR（llvm 方言）─ Backend ─▶ 机器码 + 要回填的地址
```

机器码是 System V 函数 `extern "C" fn(regs: *mut Value, acc: *mut Value, start: u64) -> u64`，直接在本帧的寄存器文件与累加器上跑；
返回 0 表示函数返回了，返回 `pc + 1` 表示停在第 `pc` 条之前，由解释器接着跑。

- 第一层接受：寄存器读写、数字与布尔常量、算术、比较、`++` / `--`、跳转、TDZ 检查、`return`；其余（调用、属性、闭包……）停在自己之前
- `start` 选入口：0 从头，`h + 1` 从循环头 `h` 进（栈上替换，OSR）

由 `j8jit` 使用，宿主一般不直接调用。

## 许可

MIT，随 [j8](https://github.com/0x00-Community/j8) 一起。
