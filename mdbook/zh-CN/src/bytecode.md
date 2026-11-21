# 字节码：与 V8 Ignition 逐字节一致

`crates/j8bytecode` 只定义字节码，JS ⇒ 字节码归 [j8compiler](compiler.md)。它是 j8 的**依赖根**：不依赖任何其他 j8
crate（只用 `j8macros` 过程宏生成表格）。

## 为什么用 V8 的字节码

j8 早期有自己的字节码（由一份 `insn.json` 生成）。后来整套换成了 **V8 Ignition 的字节码，逐字节一致**：

- 字节码设计是 V8 多年调出来的结果：累加器式、短 `Star`、按参数个数分的 `Call` 变体、反馈槽……照搬比重新发明可靠；
- 能和 V8 对拍：同一段 JS，`j8bcdbg compile` 的输出可以直接和 `d8 --print-bytecode` 比；
- JIT 的推测依据（反馈槽的种类与编号）也照 V8 排，读起来有现成的参照。

当前对齐 **V8 15.2.124**（rusty_v8 152.2.0 里的那一版）。

## 表格从 V8 头文件生成

```text
V8 检出 ─ tools/v8bytecodes.py ─▶ crates/j8bytecode/v8.json ─ j8macros::bytecodes! ─▶ Rust 类型
```

`v8.json` 的来源是 V8 的 `src/interpreter/bytecodes.h`、`bytecode-operands.h`、`interpreter-intrinsics.h`、
`src/runtime/runtime.h`、`function-kind.h`、`feedback-vector.h` 等头文件，包括：

| JSON 键 | 生成什么 |
|---|---|
| `bytecodes` | `Bytecode`（`repr(u8)`，V8 的编号，共 215 个）与每个字节码的 `operand_types()`、`implicit_register_use()` |
| `operand_types` | `OperandType`、`OperandTypeInfo` |
| `implicit_register_use` | 是否读 / 写 / 破坏累加器、是否写短 `Star` |
| `runtime` / `intrinsics` | `CallRuntime` / `InvokeIntrinsic` 的函数编号 |
| `function_kinds`、`feedback_slot_kinds`、`scope_types`、`variable_modes` | 对应的 V8 枚举 |

⛔ 手改 JSON 或生成的表格：要换 V8 版本，就拿新检出重新跑 `tools/v8bytecodes.py`。

## 编码

和 V8 一样：

- 一条指令是一个字节码字节，后面跟小端的操作数；
- **可放宽**的操作数缺省占 1 字节，前面加 `Wide` / `ExtraWide` 前缀时占 2 / 4 字节（`OperandScale`）；
  固定大小的操作数总是占它的大小；
- `Encoder` 为每条指令选最小的放宽，`Decoder` 把字节读回指令。

`OperandScale::for_signed` / `for_unsigned` 对应 V8 的 `ScaleForSignedOperand` / `ScaleForUnsignedOperand`。

## 寄存器与帧布局

累加器式寄存器机：大多数运算的结果进隐式的累加器 `acc`，二元运算是 `acc = reg op acc`。

寄存器操作数是**它相对帧指针的槽号**（`Register`，V8 的 x64 解释器帧，`InterpreterFrameConstants`）：

```text
fp + 2 …   参数（第一个是 receiver，即 this）
fp + 1     调用者 pc
fp + 0     调用者 fp
fp − 1     上下文
fp − 2     闭包（当前函数）
fp − 3     实参个数
fp − 4     字节码数组
fp − 5     字节码偏移
fp − 6     反馈向量
fp − 7 …   r0, r1, r2 …
```

所以反汇编里看到的 `a0`、`a1` 是参数，`r0` 起是局部寄存器。`Star0` … `Star15` 是十六个「短 Star」：
不带操作数、直接写 r0 … r15 的单字节指令（`Bytecode::short_star`）。

## 编译产物的形状

照 V8 的对象来（`function.rs`）：

| j8 | V8 | 内容 |
|---|---|---|
| `Module` | — | 一份源码编出的全部函数，第一个是顶层 |
| `SharedFunction` | `SharedFunctionInfo` | 名字、种类（`FunctionKind`）、是否严格、`length`、形参个数、作用域信息、字节码 |
| `BytecodeArray` | `BytecodeArray` | 代码字节、常量池、异常处理表、参数 / 寄存器个数、反馈槽种类、new.target / 生成器寄存器 |
| `Constant` | 常量池项 | 字符串、数、作用域信息、对象 / 数组 / 类的样板、模板对象…… |
| `HandlerEntry` | 处理表项 | `[start, end)` 范围、处理位置、上下文寄存器、捕获预测 |

`Flag8` 操作数（如 `CreateClosureFlags`、`CreateObjectLiteralFlags`）的位布局照 V8 的 `bytecode-flags-and-tokens.h`。

## 看一眼真实的字节码

```js
function add(a, b) { return a + b; }
let s = 0;
for (let i = 0; i < 10; i++) s = add(s, i);
s
```

`j8bcdbg compile` 的输出（格式照 V8 的 `--print-bytecode`）：

```text
== #0 NormalFunction params 1 regs 5
  @   0 LdaConstant [0]
  @   2 Star1
  @   3 Mov <closure>, r2
  @   6 CallRuntime #375, r1, #2          ; DeclareGlobals：声明顶层函数 add
  @  11 LdaZero
  @  12 StaCurrentContextSlot #2          ; let s = 0（顶层 let 在脚本上下文里）
  @  14 LdaZero
  @  15 Star1                             ; i = 0，放寄存器 r1
  @  16 LdaSmi [10]
  @  18 TestLessThan r1, #0               ; i < 10，#0 是反馈槽
  @  21 JumpIfFalse [28]
  @  23 LdaGlobal [1], FBV[0]             ; 读全局 add
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
  @  45 JumpLoop [29], [0], FBV[5]        ; 回跳：OSR 的入口
  @  49 LdaCurrentContextSlot #2
  @  51 Star0
  @  52 Return
== add NormalFunction params 3 regs 0
  @   0 Ldar a1
  @   2 Add a0, #0
  @   5 Return
```

几个值得注意的地方：

- `CallUndefinedReceiver2`：两个实参、receiver 是 `undefined` 的调用，有专门的字节码；
- 每个运算都带反馈槽（`#0`、`FBV[2]`）：执行器在这里记录见过的类型与 shape，JIT 据此推测；
- `JumpLoop` 是唯一往回跳的指令，执行器在这里计数，热了就问 JIT 要不要从这里切进机器码（OSR）。

## 测试

`tests/v8.rs` 解码 V8 15.2.124 用 `--print-bytecode` 打印的 738 条指令，逐条核对名字、长度和寄存器操作数。
