# Architecture: the life of a script

## The crates

```text
                       j8 (facade: re-exports by feature, binding macros and runtime!)
                                        │
   ┌──────────────┬───────────────┬─────┴──────┬─────────────┬────────────┐
 j8jit        j8executor      j8builtins   j8platform    j8binding     j8napi
   │   ╲          │   ╲            │            │             │            │
   │    ╲     j8compiler ╲         │            │             │            │
   │     ╲        │       ╲        │            │             │            │
   │      └── j8bytecode ──┴───────┼────────────┼─────────────┼────────────┘
   │                               │            │             │
   └───────────────────────── j8framework ──────┴─────────────┘
                                   │
                    paragc · j8taskqueue · j8lockfree · j8macros
```

| crate | Role |
|---|---|
| `j8framework` | the core: the traits each layer implements (`Platform`, `Invoke`, `Intrinsics`) and the shared structs (`Isolate`, `Scope`, `Agent`, `Context` / `Realm`, `Value`, the object model, host objects). JS operation semantics hang off `Agent`. Depends on no other layer |
| `j8bytecode` | bytecode definitions, byte for byte V8 Ignition's; the dependency root, depends on no other j8 crate |
| `j8compiler` | JS ⇒ bytecode (oxc parses; code generation after V8's `BytecodeGenerator`) |
| `j8executor` | the bytecode executor `Executor<'p>` (`eval`, resumable `start` / `resume`), with inline caches, sliced execution, the JIT hook and the event loops that drive it |
| `j8jit` | the speculative JIT (x86-64 Unix and Windows): implements the executor's `Tier` |
| `j8builtins` | the standard built-in objects: implements `Intrinsics` |
| `j8platform` | the standard-library `Platform`: thread pool, clocks, module loading |
| `j8binding` | the runtime side of Rust ⇄ JS bindings (`FromJs` / `IntoJs`, host objects) |
| `j8napi` | Node-API: the C interface native addons link against |
| `j8taskqueue` | a lock-free dataflow task queue (`no_std`); every event loop is one |
| `j8lockfree` | hand-written lock-free building blocks (`no_std`) |
| `j8macros` | procedural macros: bytecode tables, heap types, binding macros |

The rule for dependencies: **each layer depends only on `j8framework`** (the executor also on the compiler and the
bytecode, the JIT on the executor), and `j8framework` depends on no layer, only on paragc and two building blocks. So
the built-ins don't know the executor and the platform doesn't know the built-ins: they are all written against the
traits in `j8framework`.

## Traits are the seams

`j8framework` defines the interfaces, the layers implement them, the host puts them together:

| trait | implemented by | does |
|---|---|---|
| `Platform` (supertrait `Pool`) | `j8platform::DefaultPlatform`, or the host's own | thread pool, clocks, `console.log` output, module loading |
| `Intrinsics` | `j8builtins::Builtins` | installs the global object and built-in constructors into a new `Realm` |
| `Invoke` | the executor | built-ins calling back into JS (`map`'s callback, getters …) |
| `Tier` | `j8jit::Jit` | the executor asks "compile this function / loop?" |

## The life of a script

Take `dune -p 'let s = 0; for (let i = 0; i < 1e6; i++) s += i; s'`:

1. **The host sets up.** The runtime dune declares with `j8::runtime!` creates an `Isolate` (one GC heap), an
   `Executor` and a `Context` (the global object installed by `Builtins`) on an event loop thread, and runs the JS
   preludes ([Building a host](embedding.md)).
2. **Compile.** The executor hands the source to `j8compiler`: oxc parses and checks it (early errors are
   `SyntaxError`s here), scope analysis decides whether each variable lives in a register, a context slot or on the
   global object, and the generator emits bytecode after V8's `BytecodeGenerator`. The result is a
   `j8bytecode::Module` (a list of `SharedFunction`s, each with a `BytecodeArray`). When many runtimes run the same
   preludes, a `CodeCache` compiles each source once ([Executor](executor.md)).
3. **Interpret.** The executor decodes the bytecode into threaded code: every instruction carries the handler that runs
   it, and handlers jump straight to each other. Property accesses hit inline caches keyed by shape. Every thousand or
   so instructions it checks the event queue, every ten thousand it yields (slicing), giving the heap a chance to
   collect.
4. **Tier up.** Every backward jump of the loop is counted. Past the threshold the executor asks the `Tier`: the JIT
   builds the function into a speculative SSA graph from the inline caches' feedback (`s` and `i` guessed as int32),
   optimizes it, emits x86-64 directly and enters it at the loop header (OSR).
5. **Deoptimize.** When a speculation fails (`s` outgrows int32), a guard jumps to a cold path; `j8_deopt` rebuilds the
   interpreter frames from the recorded frame state and the interpreter goes on. The JIT leaves a hint: next time it
   uses doubles there.
6. **Finish.** The result is a `Value` (one NaN-boxed machine word); the host gets a `ScopeGlobal` root, applies ToString
   and prints it. No host work is left on the event loop, so the process exits.

The following chapters walk that path: [bytecode](bytecode.md) → [compiler](compiler.md) → [executor](executor.md) →
[JIT](jit.md), then what they all stand on: the [framework](framework.md), the [object model](objects.md) and the
[heap](heap.md).
