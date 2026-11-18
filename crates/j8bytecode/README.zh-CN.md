# j8bytecode

[English](README.md) · **简体中文**

[j8](https://github.com/0x00-Community/j8) 的字节码，与 V8（Ignition）逐字节一致。只定义字节码，JavaScript ⇒ 字节码归
`j8compiler`。它是 j8 的依赖根：不依赖任何其他 j8 crate（只用 `j8macros` 过程宏）。

- 字节码表（V8 的编号）、操作数类型、隐式读写累加器来自 V8 的 `src/interpreter/bytecodes.h` / `bytecode-operands.h`：
  `tools/v8bytecodes.py <v8 检出>` 写出 `v8.json`，`j8macros::bytecodes!` 生成 `Bytecode` / `OperandType` /
  `OperandTypeInfo` / `ImplicitRegisterUse`。当前对齐 V8 15.2.124（rusty_v8 152.2.0 里的那版）
- 编码同 V8：一字节字节码，后面是小端操作数。可放宽的操作数占 1 字节，前面加 `Wide` / `ExtraWide` 前缀时占 2 / 4 字节
  （`OperandScale`）；`Encoder` 每条指令取最小放宽，`Decoder` 读回指令
- 寄存器操作数就是它相对帧指针的槽号（`Register`，V8 的 x64 解释器帧）：r0 是 -7，receiver 是 +2，上下文 -1，函数 -2
- `tests/v8.rs` 解码 V8 15.2.124 打印（`--print-bytecode`）的 738 条指令，逐条核对名字、长度和寄存器操作数

## 许可

MIT，随 [j8](https://github.com/0x00-Community/j8) 一起。
