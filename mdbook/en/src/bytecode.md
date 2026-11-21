# Bytecode: V8 Ignition, byte for byte

`crates/j8bytecode` only defines the bytecode; JS ⇒ bytecode belongs to the [compiler](compiler.md). It is j8's
**dependency root**: it depends on no other j8 crate (it only uses the `j8macros` procedural macros for its tables).

## Why V8's bytecode

j8 used to have a bytecode of its own (generated from an `insn.json`). It was replaced wholesale by **V8's Ignition
bytecode, byte for byte**:

- the design is the result of years of tuning in V8 (accumulator machine, short `Star`s, `Call` forms by argument count,
  feedback slots); adopting it is safer than reinventing it;
- it can be compared with V8 directly: `j8bcdbg compile` prints the same listing as `d8 --print-bytecode`;
- what the JIT speculates on (the kinds and numbering of feedback slots) follows V8 too, so there is a reference to read.

j8 tracks **V8 15.2.124** (the one in rusty_v8 152.2.0).

## Tables generated from V8's headers

```text
V8 checkout ─ tools/v8bytecodes.py ─▶ crates/j8bytecode/v8.json ─ j8macros::bytecodes! ─▶ Rust types
```

`v8.json` comes from V8's `src/interpreter/bytecodes.h`, `bytecode-operands.h`, `interpreter-intrinsics.h`,
`src/runtime/runtime.h`, `function-kind.h`, `feedback-vector.h` and others:

| JSON key | Generates |
|---|---|
| `bytecodes` | `Bytecode` (`repr(u8)`, V8's numbering, 215 of them) with each one's `operand_types()` and `implicit_register_use()` |
| `operand_types` | `OperandType`, `OperandTypeInfo` |
| `implicit_register_use` | whether it reads / writes / clobbers the accumulator, writes a short `Star` |
| `runtime` / `intrinsics` | the function ids of `CallRuntime` / `InvokeIntrinsic` |
| `function_kinds`, `feedback_slot_kinds`, `scope_types`, `variable_modes` | the matching V8 enums |

⛔ editing the JSON or the generated tables by hand: for a new V8 version, rerun `tools/v8bytecodes.py` on a checkout.

## Encoding

As in V8:

- an instruction is a bytecode byte followed by its operands, little-endian;
- **scalable** operands take 1 byte, or 2 / 4 bytes after a `Wide` / `ExtraWide` prefix (`OperandScale`); fixed ones
  always take their size;
- `Encoder` picks the smallest scale per instruction, `Decoder` reads instructions back.

`OperandScale::for_signed` / `for_unsigned` are V8's `ScaleForSignedOperand` / `ScaleForUnsignedOperand`.

## Registers and the frame

An accumulator register machine: most operations leave their result in the implicit accumulator `acc`; a binary
operation is `acc = reg op acc`.

A register operand is **its slot relative to the frame pointer** (`Register`, V8's x64 interpreter frame,
`InterpreterFrameConstants`):

```text
fp + 2 …   parameters (the receiver, i.e. this, first)
fp + 1     caller pc
fp + 0     caller fp
fp − 1     context
fp − 2     closure (the current function)
fp − 3     argument count
fp − 4     bytecode array
fp − 5     bytecode offset
fp − 6     feedback vector
fp − 7 …   r0, r1, r2 …
```

In listings, `a0`, `a1` are parameters and `r0` up are locals. `Star0` … `Star15` are the sixteen short `Star`s:
one-byte instructions without operands that write r0 … r15 (`Bytecode::short_star`).

## The shape of compiled code

After V8's objects (`function.rs`):

| j8 | V8 | Contents |
|---|---|---|
| `Module` | — | every function compiled from one source, the first being the top level |
| `SharedFunction` | `SharedFunctionInfo` | name, kind (`FunctionKind`), strictness, `length`, formal parameter count, scope info, bytecode |
| `BytecodeArray` | `BytecodeArray` | code bytes, constant pool, handler table, parameter / register counts, feedback slot kinds, the new.target / generator register |
| `Constant` | constant pool entry | strings, numbers, scope infos, object / array / class boilerplates, template objects … |
| `HandlerEntry` | handler table entry | `[start, end)`, handler offset, context register, catch prediction |

The layouts of `Flag8` operands (`CreateClosureFlags`, `CreateObjectLiteralFlags` …) follow V8's
`bytecode-flags-and-tokens.h`.

## Real bytecode

```js
function add(a, b) { return a + b; }
let s = 0;
for (let i = 0; i < 10; i++) s = add(s, i);
s
```

What `j8bcdbg compile` prints (the format of V8's `--print-bytecode`):

```text
== #0 NormalFunction params 1 regs 5
  @   0 LdaConstant [0]
  @   2 Star1
  @   3 Mov <closure>, r2
  @   6 CallRuntime #375, r1, #2          ; DeclareGlobals: declares the top-level function add
  @  11 LdaZero
  @  12 StaCurrentContextSlot #2          ; let s = 0 (top-level lets live in the script context)
  @  14 LdaZero
  @  15 Star1                             ; i = 0, in register r1
  @  16 LdaSmi [10]
  @  18 TestLessThan r1, #0               ; i < 10, #0 is the feedback slot
  @  21 JumpIfFalse [28]
  @  23 LdaGlobal [1], FBV[0]             ; read the global add
  @  26 Star2
  @  27 LdaCurrentContextSlot #2
  @  29 Star3
  @  30 Ldar r1
  @  32 Star4
  @  33 CallUndefinedReceiver2 r2, r3, r4, FBV[2]
  @  38 StaCurrentContextSlot #2
  @  40 Ldar r1
  @  42 Inc FBV[4]
  @  44 Star1
  @  45 JumpLoop [29], [0], FBV[5]        ; the back edge: where OSR enters
  @  49 LdaCurrentContextSlot #2
  @  51 Star0
  @  52 Return
== add NormalFunction params 3 regs 0
  @   0 Ldar a1
  @   2 Add a0, #0
  @   5 Return
```

Worth noticing:

- `CallUndefinedReceiver2`: a call with two arguments and an `undefined` receiver has a bytecode of its own;
- every operation carries a feedback slot (`#0`, `FBV[2]`): the executor records the types and shapes it sees there,
  and the JIT speculates on them;
- `JumpLoop` is the only backward jump; the executor counts it, and when it is hot asks the JIT whether to enter
  machine code there (OSR).

## Tests

`tests/v8.rs` decodes 738 instructions that V8 15.2.124 printed with `--print-bytecode` and checks each one's name,
length and register operands.
