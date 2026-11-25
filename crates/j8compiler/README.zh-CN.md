# j8compiler

[English](README.md) · **简体中文**

[j8](https://github.com/0x00-Community/j8) 的编译器：JavaScript ⇒ j8 字节码（`j8bytecode::Module`），用
[oxc](https://oxc.rs) 解析。所有 JS 运行前都先过这里——脚本、ES 模块、动态代码（`eval`、`Function`）；不设 AST 解释层。

WebAssembly 走同一条路：`CompileWasm::compile_wasm` 用 [wasmparser](https://crates.io/crates/wasmparser)
（WebAssembly 3.0 特性）校验二进制模块，把每个函数体编成 j8 的 WebAssembly 寄存器字节码（`j8bytecode::WasmModule`）。
操作数栈的每个槽是局部变量之上的固定寄存器，所以块的结果本来就在该在的位置，只有跳转需要搬值。`local.get` 压的是对局部变量的
引用而不是拷贝（只在控制流、调用或写这个局部变量之前才落地成拷贝），紧跟在一条指令后的 `local.set` 直接把那条指令的目的改成局部变量。常量表达式（全局 /
表的初值、段偏移和段元素）也编成小函数。类型保留递归分组和声明的父类型；`try_table` 编成处理表，每个 catch 子句一段落地代码。

```text
源码 ─ oxc 解析 + 语义检查 ─▶ AST ─ 作用域解析 ─ 生成代码 ─ 窥孔融合 ─▶ Module
```

## 用法

```rust
use j8compiler::Compile;
use j8framework::{CodeKind, Source};

let module = j8bytecode::Module::compile(&Source { text: "1 + 2", file: "a.js" }, CodeKind::Script)?;
let (module, info) = j8bytecode::Module::compile_module(&source)?;  // ES 模块，连同导入 / 导出信息
```

`Compile::compile_direct` 按调用点的作用域表编直接 eval 的代码。这些都由 `j8executor` 自己调用，宿主一般不直接用。

## 许可

MIT，随 [j8](https://github.com/0x00-Community/j8) 一起。
