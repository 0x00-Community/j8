# j8bytecode

[English](README.md) · **简体中文**

[j8](https://github.com/0x00-Community/j8) 的虚拟指令集：累加器式寄存器字节码（同 V8 Ignition）。只定义字节码，不解析
JavaScript（那是 `j8compiler`）。它是 j8 的依赖根：不依赖任何其他 j8 crate（只用 `j8macros` 过程宏）。

- 全部类型定义在 `insn.json`，由 `j8macros::enums!` 生成（JSON 是唯一来源）：指令 `Insn`、操作符
  `BinaryOp` / `CompareOp` / `UnaryOp`、`Constant`、`FunctionKind`、`PropertyKind`、`Mode`，以及结构体
  `Reg` / `Label` / `Const` / `EvalName` / `EvalScope` / `Function` / `Module`
- 汇编与反汇编互为逆：`text.parse::<Module>()` / `module.to_string()`
- 相邻指令窥孔融合：`Function::fuse`

## 用法

```rust
use j8compiler::Compile;
let module = j8bytecode::Module::compile(&source, j8framework::CodeKind::Script)?; // JS ⇒ 字节码归 j8compiler
println!("{module}");                       // 反汇编清单
let again: j8bytecode::Module = module.to_string().parse()?;
```

命令行工具 `j8dbg`（`debugger/`）可以打印、汇编清单。

## 许可

MIT，随 [j8](https://github.com/0x00-Community/j8) 一起。
