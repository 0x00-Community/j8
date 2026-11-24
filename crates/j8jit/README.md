# j8jit

**English** · [简体中文](README.zh-CN.md)

The JIT of [j8](https://github.com/0x00-Community/j8) (x86-64 Unix and Windows, AArch64 Linux). It owns bytecode ⇒ machine code and the JIT
scheduling itself: it implements the executor's `Tier`; hot functions and hot loops are built into a speculative SSA graph
(`build.rs`), which becomes machine code in one of two ways, is loaded into executable memory and runs in place of interpretation.

```text
j8bytecode::Function ─ build ─▶ graph ─┬─ codegen.rs ──────────────────────────────▶ machine code (default; x86-64, AArch64)
                                       └─ stitch.rs: copy stencils, patch holes ─▶ machine code (J8_JIT_MLIR=1; x86-64, AArch64)
templates (one per graph instruction) ─ mlirpass pipeline + x86_64.rs / aarch64.rs, once per process ─▶ stencils
```

The stitched path is the [mlirpass](https://crates.io/crates/mlirpass) fast path (`Stencils`): every template goes through
the pipeline and the x86-64 or AArch64 backend once, the first time the JIT is used; a function is then compiled by copying stencils
and filling their holes (frame slots, constants, jumps to deoptimization exits), with no MLIR built per function.

```text
calls / iterations below the threshold  ⇒ count, interpret as usual
threshold reached                       ⇒ compile (a failure is remembered and never retried)
entering the frame                      ⇒ run the machine code on this frame's registers
a loop back-jump                        ⇒ switch into machine code at that loop header (OSR)
stopped before some instruction         ⇒ the interpreter continues from there
```

## Usage

```rust
use std::rc::Rc;
let executor = j8executor::Executor::default().with_tier(Rc::new(j8jit::Jit::default()));
```

`Jit::from(n)` sets the call threshold (`Jit::THRESHOLD` = 64 by default; 0 compiles every function on its first call). `dune --jit` and `J8_TEST262_JIT=1` turn it on.

## WebAssembly

`WasmJit` is the tier for WebAssembly (`WasmStore::with_tier`, or `WebAssembly::from(builtins).with_tier(Arc::new(WasmJit::default()))`
for the JS API). It uses the same stencil fast path: one template per kind of wasm bytecode, compiled through mlirpass once
per process; a function (`WasmJit::THRESHOLD` = 16 calls) is then copied together from stencils with its registers in the
native frame. Integer and f64 arithmetic, comparisons, constants, branches, `br_table`, `select`, linear-memory loads and
stores run in machine code; other scalar operators call one numeric helper; direct calls go straight into the callee's
machine code; anything else (globals, tables, GC, indirect calls, throws) is one interpreter step on the same registers.
Functions that touch `v128`, have exception handlers or use `br_on_cast` stay interpreted.

## License

MIT, as part of [j8](https://github.com/0x00-Community/j8).
