# j8compiler

**English** · [简体中文](README.zh-CN.md)

The compiler of [j8](https://github.com/0x00-Community/j8): JavaScript ⇒ j8 bytecode (`j8bytecode::Module`), parsed
with [oxc](https://oxc.rs). Every piece of JS goes through here before it runs — scripts, ES modules, and dynamic code
(`eval`, `Function`); there is no AST interpreter.

WebAssembly goes the same way: `CompileWasm::compile_wasm` validates a binary module with
[wasmparser](https://crates.io/crates/wasmparser) (WebAssembly 3.0 features) and turns every function body into
j8's WebAssembly register bytecode (`j8bytecode::WasmModule`). Each operand-stack slot is a fixed register above the
locals, so block results already sit where they are expected; only branches move values. Constant expressions
(global / table initializers, segment offsets and elements) are compiled into small functions too. GC types and exception
handling are reported as `WasmErrorKind::Unsupported` for now.

```text
source ─ oxc parser + semantic checks ─▶ AST ─ scope resolution ─ code generation ─ peephole fusion ─▶ Module
```

## Usage

```rust
use j8compiler::Compile;
use j8framework::{CodeKind, Source};

let module = j8bytecode::Module::compile(&Source { text: "1 + 2", file: "a.js" }, CodeKind::Script)?;
let (module, info) = j8bytecode::Module::compile_module(&source)?;  // an ES module plus its imports / exports
```

`Compile::compile_direct` compiles direct-eval code against the call site's scope table. `j8executor` calls all of
these itself; embedders normally don't.

## License

MIT, as part of [j8](https://github.com/0x00-Community/j8).
