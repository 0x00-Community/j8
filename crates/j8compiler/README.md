# j8compiler

**English** · [简体中文](README.zh-CN.md)

The bytecode compiler of [j8](https://github.com/0x00-Community/j8)'s JIT: one bytecode function ⇒ MLIR (`llvm`
dialect, via [mlirpass](https://crates.io/crates/mlirpass)) ⇒ machine code through a `Backend` (the x86-64 one lives in
`j8jit`).

```text
j8bytecode::Function ─ Lower ─▶ MLIR (llvm dialect) ─ Backend ─▶ machine code + addresses to patch
```

The machine code is a System V function `extern "C" fn(regs: *mut Value, acc: *mut Value, start: u64) -> u64`. It runs
on the frame's own register file and accumulator; it returns 0 when the function returned, or `pc + 1` when it stopped
before instruction `pc` and the interpreter continues from there.

- The first tier takes register moves, number and boolean constants, arithmetic, comparisons, `++` / `--`, jumps, TDZ
  checks and `return`; anything else (calls, properties, closures …) stops before itself
- `start` picks where to enter: 0 at the start, `h + 1` at loop header `h` (on-stack replacement)

Used by `j8jit`; embedders normally don't call it directly.

## License

MIT, as part of [j8](https://github.com/0x00-Community/j8).
