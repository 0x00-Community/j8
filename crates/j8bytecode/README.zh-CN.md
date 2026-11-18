# j8bytecode

[English](README.md) · **简体中文**

[j8](https://github.com/0x00-Community/j8) 的虚拟指令集：累加器式寄存器字节码（同 V8 Ignition）。只定义字节码，不解析
JavaScript（那是 `j8compiler`）。它是 j8 的依赖根：不依赖任何其他 j8 crate（只用 `j8macros` 过程宏）。

- 全部类型定义在 `insn.json`，由 `j8macros::enums!` 生成（JSON 是唯一来源）：指令 `Insn`、操作符
  `BinaryOp` / `CompareOp` / `UnaryOp`、`Constant`、`FunctionKind`、`PropertyKind`、`Mode`，以及结构体
  `Reg` / `Label` / `Const` / `IcSlot` / `EvalName` / `EvalScope` / `Draft` / `Function` / `Module`，操作码 `Opcode`
- 函数的代码就是字节（`Function::code`）：每条指令一字节操作码加定宽小端操作数，末尾补 `Insn::PAD` 字节；程序计数器和
  跳转目标都是字节偏移。j8compiler 先拼 `Draft`（`Vec<Insn>`），融合（`Draft::fuse`）后汇编（`Box::<[u8]>::from(draft)`：
  跳转目标改成字节偏移、内联缓存槽 `IcSlot` 编号）。执行器和 JIT 按偏移解码：`Function::insn_at(pc)` /
  `Opcode::at(code, pc)`；解释器用 `j8macros::dispatch!` 每条指令只按操作码分派一次
- 没有文本形式：字节码的存储与分发只用二进制形式，无损：`Vec::<u8>::from(&module)` / `Module::try_from(bytes)`。开头是
  `Module::MAGIC`（`\0j8b`，`Module::is_binary` 只看它）和指令集指纹 `SCHEMA`，别的 `insn.json` 出的字节码直接拒。
  函数代码原样带过去，读入时校验一遍（每条指令能解、每个跳转落在指令开头）

## 用法

```rust
use j8compiler::Compile;
let module = j8bytecode::Module::compile(&source, j8framework::CodeKind::Script)?; // JS ⇒ 字节码归 j8compiler
let bytes = Vec::<u8>::from(&module);       // 二进制形式
let again = j8bytecode::Module::try_from(bytes.as_slice())?;
```

命令行工具 `j8dbg`（`debugger/`）把 JS 编成二进制形式（`compile`）、读回校验（`check`）。

## 许可

MIT，随 [j8](https://github.com/0x00-Community/j8) 一起。
