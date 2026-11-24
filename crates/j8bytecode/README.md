# j8bytecode

**English** · [简体中文](README.zh-CN.md)

The bytecode of [j8](https://github.com/0x00-Community/j8), which is V8's (Ignition) byte for byte. It only defines
the bytecode; JavaScript ⇒ bytecode is `j8compiler`. It is the dependency root of j8: it depends on no other j8 crate
(only the `j8macros` procedural macros).

- The bytecode list (V8's numbering), operand types and implicit accumulator use come from V8's
  `src/interpreter/bytecodes.h` / `bytecode-operands.h`: `tools/v8bytecodes.py <v8 checkout>` writes `v8.json`,
  `j8macros::bytecodes!` turns it into `Bytecode` / `OperandType` / `OperandTypeInfo` / `ImplicitRegisterUse`.
  Currently V8 15.2.124 (the V8 inside rusty_v8 152.2.0)
- Encoding as V8: a bytecode byte, then its operands little-endian. Scalable operands take 1 byte, or 2 / 4 bytes
  after a `Wide` / `ExtraWide` prefix (`OperandScale`); `Encoder` picks the smallest scale per instruction, `Decoder`
  reads instructions back
- A register operand is the register's slot from the frame pointer (`Register`, V8's x64 interpreter frame): r0 is
  -7, the receiver +2, the context -1, the closure -2
- WebAssembly gets its own bytecode table, `WasmBytecode`: a three-address register form of the WebAssembly 3.0
  instruction set. `tools/wasmbytecodes.py <v8 checkout>` writes `wasm.json` from V8's `src/wasm/wasm-opcodes.h` (V8's
  operator names, `wasm()` / `from_wasm()` map to V8's `WasmOpcode` numbering), `j8macros::wasm_bytecodes!` turns it
  into `WasmBytecode` / `WasmOperandType`. Operators with a signature take one register per result and parameter, then
  their immediates; structured control, `local.*`, `drop` and `try_table` become `Jump*` / `Mov` / the handler table.
  Page 0 is one byte, SIMD (page 1) follows the `Simd` prefix byte; `Wide` / `ExtraWide` scale operands as in V8.
  `Encoder` / `Decoder` work on both tables (`Opcode`)
- `tests/v8.rs` decodes 738 instructions V8 15.2.124 printed (`--print-bytecode`) and checks every name, length and
  register operand

## License

MIT, as part of [j8](https://github.com/0x00-Community/j8).
