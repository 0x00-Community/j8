# j8jit

**English** · [简体中文](README.zh-CN.md)

The JIT of [j8](https://github.com/0x00-Community/j8) (x86-64 Unix). It implements the executor's `Tier`: hot
functions and hot loops are compiled by `j8compiler`, emitted by this crate's x86-64 backend (`x86_64.rs`), loaded into
executable memory and run in place of interpretation.

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

`Jit::from(n)` sets the call threshold (`Jit::THRESHOLD` = 100 by default; loops take `LOOP_FACTOR` times as many
iterations; 0 compiles every function on its first call). `dune --jit` and `J8_TEST262_JIT=1` turn it on.

## License

MIT, as part of [j8](https://github.com/0x00-Community/j8).
