# j8executor

**English** · [简体中文](README.zh-CN.md)

The bytecode executor of [j8](https://github.com/0x00-Community/j8): it runs the accumulator-style register
bytecode of `j8bytecode` (`Executor<'p>`: `eval`, resumable `start` / `resume`) and drives the JS event loops
(`EventLoop` / `EventLoopSet`).

`WasmStore` runs WebAssembly: host functions / tables / memories / globals, `instantiate` (import linking, globals,
tables, memories, element and data segments, start function) and `invoke`. The interpreter decodes each function's
bytecode once, keeps 64-bit register slots on one stack (a callee's frame starts at the caller's argument registers, so
arguments and results are never copied), uses an explicit frame stack, and traps with `WasmTrap`.

## How it runs

- An explicit frame stack (no host recursion); all frames share one register file; captured bindings live in the
  heap-allocated `Env` chain
- Exceptions: each frame has a dynamic handler stack; a throw pops this frame's handler, otherwise unwinds to the caller
- Inline caches (`ic.rs`) for property reads, writes and property additions, keyed by shape address
- Sliced execution: `Executor::eval` runs at most a fixed number of instructions per slice, parks the state under a root,
  lets the heap collect, and continues
- A `Tier` hook lets a JIT (`j8jit`) take over hot functions and loops

An `Executor` serves one isolate: it holds the loaded functions, and closures on the heap point back to them by `FuncId`.

## Usage

```rust
let mut executor = j8executor::Executor::default();
let result = executor.eval(&mut isolate, &cx, &j8framework::Source { text: "1 + 2", file: "a.js" })?;
```

## License

MIT, as part of [j8](https://github.com/0x00-Community/j8).
