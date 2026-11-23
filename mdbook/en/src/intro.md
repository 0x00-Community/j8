# What j8 is

j8 is a **JavaScript engine written from scratch in Rust**. Apart from the parser ([oxc](https://oxc.rs)), everything
from the bytecode, the executor and the JIT to the garbage collector is its own code: there is no C++ engine
underneath.

It follows V8's layering:

```text
JS source ─ oxc parse ─ j8compiler ─▶ V8 Ignition bytecode ─ j8executor interprets ─┐
                                                                                    ├─▶ result
             hot functions / hot loops ─ j8jit speculative compile ─▶ machine code ─┘
```

## Design choices

| | |
|---|---|
| 🦀 **All Rust** | oxc parses; everything else is j8's own code. |
| 🧱 **Bytecode first** | No AST interpreter: all JS (`eval` and `Function` included) is compiled to bytecode before it runs. |
| 🔣 **V8's bytecode** | The bytecode list, operand encoding, registers and frame layout match V8 Ignition byte for byte; the tables are generated from V8's headers. |
| 🧬 **Speculative JIT** | Feedback from the inline caches turns bytecode into a speculative SSA graph: inlining, escape analysis, loop-invariant code motion, OSR, deoptimization. |
| ♻️ **paragc heap** | Immix-style blocks and lines, young collection plus reference counting of old objects, parallel marking and sweeping; the `'scope` brand lifetime keeps handles from escaping at compile time. |
| 🚫 **No global state** | No `static`, no `'static`; background tasks borrow the caller's data, and many isolates share one thread pool. |
| 🧩 **Embeddable** | `#[j8::class]` / `#[j8::object]` / `#[j8::function]` bind a Rust type to JS in one line; `j8::runtime!` assembles a runtime. |

## The main line: test262

j8's main line is the [test262](https://github.com/tc39/test262) pass rate. The baseline `tests/test262.passing` is
**42,443** cases (sloppy plus `onlyStrict`; without Temporal, intl402 and the like, see the [Roadmap](roadmap.md)).
Every commit must keep it with zero regressions, and the same baseline runs with the JIT on.

## What is in the repository

| Directory | Contents |
|---|---|
| `crates/` | the engine itself, a dozen crates (see [Architecture](architecture.md)) |
| `src/` | the facade crate `j8`: re-exports the crates by feature, exports the binding macros |
| `dune/` | a runtime on par with Node and Bun, plus `dunx` (npx / bunx) |
| `samples/` | sample hosts and tools: workerd, jumpheap, j8bcdbg, three benchmark suites |
| `tests/` | integration tests and the test262 runner |
| `tools/` | `v8bytecodes.py`: generates the bytecode tables from V8's headers |
| `mdbook/` | this book |

## Releases

Every crate is on crates.io: the facade `j8` picks components by feature, and the sub-crates (`j8bytecode`,
`j8compiler`, `j8executor` …) are published on their own too. dune is published as `j8dune` (the name `dune` is taken).
MIT licensed.
