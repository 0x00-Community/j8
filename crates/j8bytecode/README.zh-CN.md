# j8bytecode

[English](README.md) · **简体中文**

[j8](https://github.com/0x00-Community/j8) 的虚拟指令集：累加器式寄存器字节码（同 V8 Ignition），以及 JavaScript ⇒ 字节码编译器。

- 指令枚举由 `j8macros::enums!` 从 `insn.json` 生成（JSON 是唯一来源）
- `Insn` / `Function` / `Module`；汇编与反汇编互为逆：`text.parse::<Module>()` / `module.to_string()`
- `Module::try_from(&Source)` 编译 JavaScript（用 [oxc](https://oxc.rs) 解析）；不设 AST 解释层，`eval`、`Function` 也走字节码

## 用法

```rust
let module = j8bytecode::Module::try_from(&source)?;
println!("{module}");                       // 反汇编清单
let again: j8bytecode::Module = module.to_string().parse()?;
```

命令行工具 `j8dbg`（`debugger/`）可以打印、汇编清单。

## Feature

| Feature | 缺省 | 含义 |
| --- | --- | --- |
| `compile` | 开 | JS ⇒ 字节码编译器（oxc）；关掉只剩字节码类型 |

## 许可

MIT，随 [j8](https://github.com/0x00-Community/j8) 一起。
