<a id="top"></a>
<div align="center">

# j8

### V8, rewritten in Rust.
**A bytecode interpreter, a JIT built on MLIR and a parallel garbage-collected heap ([paragc](https://crates.io/crates/paragc)).**

[![Rust](https://img.shields.io/badge/Rust-100%25-orange?style=for-the-badge&logo=rust)](https://www.rust-lang.org)
[![test262](https://img.shields.io/badge/test262-42%2C443%20passing-success?style=for-the-badge)](#-test262)
[![License](https://img.shields.io/badge/License-MIT-brightgreen?style=for-the-badge)](#-license)

[![ECMAScript](https://img.shields.io/badge/ECMAScript-core%20language-F7DF1E?style=flat-square&logo=javascript&logoColor=black)](#-status)
[![JIT](https://img.shields.io/badge/JIT-x86--64%20via%20MLIR-262D3A?style=flat-square&logo=llvm)](#-jit-tier-one)
[![GC](https://img.shields.io/badge/GC-paragc%20%C2%B7%20parallel%20%C2%B7%20lock--free-8A2BE2?style=flat-square)](#-heap-and-handles)

**🇺🇸 English** · [🇨🇳 简体中文](README.zh-CN.md)

</div>

---

## ⚡ What is j8

j8 is **a JavaScript engine written from scratch in Rust**, layered the way V8 is: every piece of JS is first compiled to
accumulator-style bytecode (like V8's Ignition) and run by the executor, while hot functions and hot loops go to a JIT that
emits x86-64 machine code. The heap is [paragc](https://crates.io/crates/paragc), wrapped by `j8framework`'s GC layer.

🎯 **The main line:** the test262 pass rate. Core ECMAScript is filled in stage by stage following [PLAN.md](PLAN.md), with
zero regressions against the baseline at every step.

🧩 **Embeddable:** `#[j8::class]` / `#[j8::object]` / `#[j8::function]` bind Rust types to JS in one line. Two sample hosts
come with it: `dune`, a runtime with a Web-standard layer and a Node-compatible layer, and `workerd`, a Cloudflare
Worker-style runtime.

## 🔥 Why it's different

| | |
| --- | --- |
| 🦀 **All Rust** | Parsing is oxc; everything else, from bytecode to GC, is ours. No C++ engine underneath. |
| 🧱 **Bytecode first** | There is no AST interpreter: all JS (including `eval` and `Function`) is compiled to bytecode. The instruction set is generated from `insn.json`, and assembling / disassembling are exact inverses. |
| 🧬 **A JIT on MLIR** | Bytecode ⇒ the MLIR `llvm` dialect ⇒ the x86-64 backend of [mlirpass](https://crates.io/crates/mlirpass) (linear-scan register allocation), with OSR. |
| ♻️ **paragc heap** | Immix-style mark-region heap: young collections plus reference counting for old objects, evacuation of sparse blocks, marking and sweeping in parallel, no locks. `'gc` brand lifetimes guarantee at compile time that handles never escape. |
| 🚫 **No `'static`** | No global state. Background tasks borrow the caller's data, and several isolates share one thread pool. |

## 🏆 Numbers

| ✔️ Check | Result |
| --- | --- |
| 📜 test262 baseline | **42,443** tests passing (sloppy mode plus `onlyStrict`; Temporal, intl402 and the like are out of scope, see PLAN.md), zero regressions on every commit |
| 🧮 JIT | `J8_TEST262_JIT=1` runs the same baseline with the JIT on (every function compiled on its first call) |
| ⏱️ vs V8 jitless | **3.7×** slower, geometric mean over 11 benchmarks; checksums identical on all three engines |
| 🚄 vs V8 (with JIT) | **9.4×** slower, geometric mean; the gap is mostly calls and allocation, see [benchmark.md](samples/benchv8/benchmark.md) |

## 🚀 Quick start

```sh
# 🟢 dune, Node-compatible (or --web for the Web-standard APIs only): run a script / evaluate / REPL
cd samples/dune
cargo run --release -- examples/node.js
cargo run --release -- --web examples/web.mjs
cargo run --release -- -p '[1, 2, 3].map(x => x * 2)'
cargo run --release                             # REPL (.help / .exit)
cargo run --release -- --jit a.js               # with the JIT

# ☁️ Cloudflare Worker style: ES modules + Durable Objects
cd samples/workerd && cargo run -- examples/counter.js --bind COUNTER=Counter --request "GET /c/inc?name=a"

# 🔬 look at the bytecode
cargo run -p j8debugger -- disasm -e 'let s = 0; for (let i = 0; i < 10; i++) s += i; s'

# 📜 the test262 regression run (git submodule update --init first)
cargo test --release --test test262

# 🥊 benchmarks against V8 (downloads rusty_v8's prebuilt library)
cd samples/benchv8 && cargo run --release
```

## 🧱 Crates

A Cargo workspace of several crates. The samples under `samples/` are packages of their own, outside the workspace,
and depend on the published `j8` by version, the way an embedder would.

| crate | std | What it does |
|---|---|---|
| `j8framework` | std | The core framework: defines the traits (`Platform`, `Engine`, `Invoke`, `Intrinsics`) and the shared structs / enums (`Isolate`, `Scope`, `Agent`, `Context` / `Realm`, `Global`, `Value`, `JsString` / `JsObject`, `Profiler`, `BinaryOp`, `Error` …). JS operation semantics hang off `Agent`; the implementations belong to the other crates |
| `j8` (repository root `src/`) | std | Re-exports: `#[j8::class]` / `#[j8::object]` / `#[j8::function]` (the macros live in j8macros, see `crates/macros/USAGE.md`) and the j8binding traits; the other crates by feature (default `runtime` = `j8::executor` + `j8::builtins` + `j8::platform`, plus `virtasm` / `taskqueue` / `lockfree` / `jit` / `full`); every crate is also published on its own on crates.io |
| `j8binding` | std | The Rust ⇄ JS binding runtime: `FromJs` / `IntoJs`, host objects, `Class` / `Object` |
| `j8builtins` | std | Implements `Intrinsics`: installs Object / Function / Array / String / Number / Boolean / Math / the Error family / `console` / `parseInt` and so on into a `Realm` |
| `j8platform` | std | Implements `Platform` (`DefaultPlatform`): background tasks run on a moremath `ThreadMulti` pool (started once and reused; tasks may borrow the caller's data, through `j8framework::Pool`), the clock is `Instant` |
| `j8virtasm` | std | The virtual instruction set: accumulator-style register bytecode (`Insn` / `Function` / `Module`); disassemble with `module.to_string()`, assemble with `text.parse::<Module>()`, each the inverse of the other. The `compile` feature (on by default) brings the JS ⇒ bytecode compiler `Module::try_from(&Source)`, parsing with oxc |
| `j8taskqueue` | `no_std` | Task queue: `ITask` (`recursable` / `depend` / `exec`) and `ITaskPoll` (`add` / `run`); lock-free dataflow scheduling (hand-written MPMC ready queue, atomic dependency counts): a task whose dependencies are done runs at once in parallel on a moremath `Executor` (the host passes `ThreadMulti`), with no round barrier; `TaskId::family` finds the union-find root, `cancel` cancels a whole family; quotas count bytecodes (`Quota` / `Flow::Yield`, a per-run budget) |
| `j8macros` | proc-macro | Procedural macros: `enums!("x.json")` generates enums from JSON (definitions, `Display`, `&mut` accessors by field type), `#[derive(Collect)]` for heap types (paragc `Trace`), and the binding macros; see `crates/macros/USAGE.md` |
| `samples/dune` | std | A runtime with two layers: `src/web/` (the Web-standard APIs: `console`, timers, `URL`, `TextEncoder`, `EventTarget` / `AbortController`, `fetch`, `structuredClone`, `crypto` …) and `src/node/` on top of it (`process`, `require` / ES modules, `Buffer`, `fs`, `path`, `os`, `events`, `util`, `assert`, `child_process` …); machine access is Rust bound with `#[j8::object]` / `#[j8::class]`, the APIs are JS preludes; `dune a.js`, `dune --web a.mjs`, `dune -e` / `-p`, a REPL; see its README |
| `samples/workerd` | std | Loads Cloudflare Workers (ES modules with `export default { fetch }`) and Durable Objects with j8: `workerd examples/counter.js --bind COUNTER=Counter --request "GET /c/inc?name=a"`, or `--port 8787` for an HTTP server; the Fetch API subset and DOs live in a JS prelude, storage is a Rust KV bound with `#[j8::class]` |
| `samples/benchv8` | std | The same JS benchmarks timed on j8 and V8 (rusty_v8 152, with JIT / `--jitless`): `cd samples/benchv8 && cargo run --release` (downloads V8's prebuilt library) |
| `j8debugger` | std | The `j8dbg` command line (clap): `j8dbg disasm <file.js \| ->` / `j8dbg disasm -e '<js>'` prints the disassembly; `j8dbg asm <file.j8s \| ->` assembles a listing and prints it normalized (`-o` writes a file) |
| `j8executor` | std | The bytecode `Executor`, **currently the last layer**; implements `Engine` under the `compile` feature |

All JS is compiled to bytecode before it runs; there is no AST interpreter.
Dependency direction: every layer depends only on `j8framework` (`j8executor` also on `j8virtasm`), and `j8framework` depends on no layer.

### 🧹 Heap and handles

The heap is [paragc](https://crates.io/crates/paragc), used only through its public API; `j8framework`'s GC layer adds what a
JS engine needs on top: `GcCell` (a lock-free `RefCell` whose data lives in a snapshot behind a paragc `Slot`: a snapshot
allocated since the last collection is changed in place, an older one is moved into a fresh snapshot first, so the write
barrier sees it), `ValueCell` (one NaN-boxed `Value`, an atomic word plus a `Slot`), `Cow`, `GcQueue`, `Static`, and
`#[derive(Collect)]` (from j8macros; `#[collect(require_static)]` skips a field or a whole type). Brand lifetimes, no `'static`.

- Collection only happens outside `enter`, when every `Gc` is under a root, so objects can move: ⛔ storing a `Gc` or an object
  address anywhere that is not traversed by `Trace` (`Static`, side caches); anything hashed by address (the indexes of Map / Set) is
  updated in `relocate` (see `Collection`)
- ⛔ writing a `Slot` / `ValueCell` / `Cow` of a value that is not on the heap yet (the barrier records the slot's address): allocate first,
  then fill in (see `Scope::context`)
- The heap only collects stop-the-world (`collect_if_needed` / `collect`, parallel inside); `GcCell` changes young snapshots without a
  barrier, so ⛔ `mark_concurrently`
- Every heap type is `Send + Sync` (paragc's rule): host data is `Box<dyn Any + Send + Sync>` behind j8lockfree's `AtomicRefCell`,
  shapes and ropes are `Arc`
- The heap has its own collector threads (as many as the platform's pool); `Platform`'s supertrait `Pool` is for parallel built-ins

- `Isolate::enter(|scope| …)` enters once, and `'gc` is the brand of that entry; nothing is collected during the closure, and after it returns the heap collects if enough was allocated (a young collection usually, a full one every few rounds)
- `Engine::eval` takes `&mut Isolate` and enters in slices itself: at most ten thousand instructions per slice; when it stops, the execution state is put under a root, and when `enter` returns a step of collection runs before execution continues. The execution context survives across slices as a `ContextRoot` (JS re-entered from a host function runs to completion within the current slice)
- `Value<'gc>` is V8's Local: it cannot leave the closure, guaranteed at compile time
- `Global` is a persistent handle (a paragc `Root`) that lives across many `enter`s and is released on drop

```rust
use j8builtins::Builtins;
use j8executor::Executor;
use j8framework::{CreateParams, Isolate, Source};
use j8platform::DefaultPlatform;

let platform = DefaultPlatform::default();
let mut isolate = Isolate::new(&platform, CreateParams::default());
let mut executor = Executor::default();
let cx = isolate.enter(|scope| scope.persist(scope.context(&Builtins)));
let result = isolate.run(&mut executor, &cx, &Source { text: "1 + 2 * 3", file: "a.js" })?;
isolate.enter(|scope| {
    let v = scope.local(&result);
});
```

## 🧬 JIT (tier one)

`dune --jit a.js` turns the JIT on; `J8_TEST262_JIT=1 cargo test --release --test test262` runs the baseline with the JIT on (every function is compiled on its first call, every loop on its first back edge).

- `j8compiler`: bytecode function ⇒ MLIR (`llvm` dialect) ⇒ mlirpass `target.x86_64` ⇒ x86-64 machine code (one `Compiler` is built and reused, loading only the builtin and `llvm` dialects).
  The machine code is `extern "C" fn(regs, acc, start) -> u64` and reads and writes the frame's register file and accumulator `Value`s directly (layout in `j8framework::ValueTag`: 16 bytes, tag in the first byte, payload from byte 8);
  `start` 0 enters at the top of the function, `h + 1` enters at loop header `h` (OSR); returning 0 means the function returned, `pc + 1` means it stopped before instruction pc (`j8executor::Exit`) and the interpreter carries on from there
- Instructions in tier one: register reads and writes, number / boolean constants, arithmetic (remainder, exponentiation and bitwise operations call runtime functions) and comparisons, `++` / `--`, jumps, TDZ checks, returns;
  when a fast-path precondition fails (an operand is not a number, say) it falls back to the interpreter; for any other instruction (calls, properties, closures …) the machine code stops before it and hands over to the interpreter
- A function that can run entirely in machine code runs it on frame entry; any other function is only entered at loop headers whose loop body hands nothing to the interpreter (OSR; entered midway, a `return` is also handed back to the interpreter to finish);
  functions with neither are not compiled
- `j8jit` (std, x86-64 Unix): implements the executor's `Tier` (`Executor::with_tier`): compiles once the call count passes a threshold (100 by default) or the loop iterations pass 10× the threshold,
  `mmap`s executable memory and fills in runtime function addresses; the executor remembers functions answered with `Tiered::Never` and never asks again
- The register file and accumulator are local variables inside the machine code (a pair of `llvm.alloca` per register: tag and payload): read from the frame once on entry, written back to the frame through one shared write-back block on every exit;
  before leaving MLIR it runs mlirpass `llvm.func(mem2reg, canonicalize, cse)`, so the locals become SSA values passed between blocks as block arguments
- The machine code backend is mlirpass (crates.io, `=0.10.4`, `target.x86_64`, with linear-scan register allocation); the MLIR uses the custom form of the `llvm` dialect (really parsed since mlirformat 0.8.1)
- Not yet: fast paths for calls / properties / global names

## 🔣 Bytecode

An accumulator register machine (like V8's Ignition): results go into the implicit `acc`, and binary operations are `acc = reg op acc`.

The instructions and every j8virtasm enum (`Insn` / `Constant` / `FunctionKind` / `PropertyKind`) are defined in `crates/j8virtasm/insn.json` and generated by `j8macros::enums!`; ⛔ hand-written enums. Adding an instruction only touches the JSON (fields of type `Label` get jump targets computed automatically and go into `target_mut`).

### Assembling / disassembling

One section per function in a listing: the function header `fn#i "name" kind mode params N length N regs N [generator] [async] [keeps_args]`, followed by indented lines in order:
`.mapped` (the mapped arguments slots, `-` for unmapped), constants `#k = number … | string "…" | bigint …`,
the scope table for direct eval `scope#k = ["x" 3 const tdz] [ … ]`, and instructions `@pc mnemonic operand, …`; `;` starts a comment to the end of the line. Numbers must be written in order.

```text
fn#0 "main" normal sloppy params 0 length 0 regs 3
  #0 = string "x"
  @0    lda.const #0
  @1    return
```

An instruction is written the same way `Display` prints it: `Insn` has `asm` turned on in insn.json, so `j8macros::enums!` also generates `FromStr for Insn`, and adding an instruction does not touch the assembler.

- Calling convention: `r0` is `this`, `r1` is the callee itself (named function expressions recurse through it), parameters start at `r2`; a `Call`'s callee, `this` and arguments sit in consecutive registers
- Scopes: a pre-pass before compiling (`Resolver` in `scope.rs`) finds bindings captured by inner functions and puts them on the heap `Env` chain (`PushEnv` / `PopEnv` / `LdaEnv depth.slot`), the rest go in registers; `for (let …)` does a `CloneEnv` every iteration so each closure gets its own iteration
- Top-level `var` / functions / `let` / `const` in a script are properties of the global object (`LdaGlobal` / `StaGlobal`)
- After compiling, every function goes through peephole fusion (`Function::fuse` in `fuse.rs`): `lda.smi n; add r` ⇒ `add.smi r, n`, `ldar y; add x` ⇒ `add.reg x, y` (comparisons likewise), `ldar a; star b` ⇒ `mov b, a`, `ldar r; inc; star r` ⇒ `inc.reg r`, `star r; ldar r` ⇒ `star r`, and of `mov b, a; mov a, b` back to back the second is dropped;
  a group is not fused if a jump lands after its first instruction, and jump targets are rewritten to the new indexes. A fused instruction means exactly the original instructions run in order (`Insn::unfused` splits it back), and the JIT lowers the split-back instructions one by one; hand-written listings are not fused
- The executor uses an explicit frame stack (no host recursion), with a call depth limit of 10000 (including JS re-entered from host functions); going over throws a RangeError
- Exceptions: one dynamic handler stack per frame (`PushHandler` / `PopHandler`); a throw pops this frame's handler, or unwinds to the caller if there is none; `finally` uses a (completion type, value) register pair, and a `return` / `break` / `continue` passing through it detours into it first and then carries on

The `javascript_semantics` test in `j8executor` is a table of JS semantics, each entry certain.

## 📋 Status

Supported:

- Literals (number / string / boolean / `null` / template strings), object literals (including method shorthand and computed keys), array literals
- All arithmetic, comparisons (`==` / `===`, strings compared by UTF-16), bitwise operations, `**`, `typeof` / `void` / `!` / `~`, `&&` / `||` / `??`, `?:`, comma
- Assignment and compound assignment (including `||=` / `&&=` / `??=`), `++` / `--`, with a name or a property as the target
- `var` / `let` / `const` (block scoping, assigning to a `const` throws TypeError), function declaration hoisting
- `if` / `while` / `do-while` / `for` / `switch` / labeled `break` / `continue` / `return` / `throw`
- Function declarations / expressions / arrow functions (lexical `this`), closures, `this` in method calls
- Property reads and writes (array indexes and `length`, string indexes and `length`, prototype chain reads)
- `try` / `catch` / `finally` (`return` / `break` / `continue` inside `finally` and jumps through `finally` all follow the spec), Error objects (`Error` / `TypeError` / `ReferenceError` / `RangeError` / `SyntaxError`)
- Builtins (j8builtins) and host functions, `console.log` (through `Platform::print`), ToPrimitive (`valueOf` / `toString`)
- `new`, classes (constructors, methods, static methods, getters / setters, `extends` / `super(...)` / `super.m()`, instance and static fields, inheriting from host constructors such as `Error`), methods and getters / setters in object literals
- Property attributes (enumerable / writable / configurable), `Object.defineProperty` / `freeze`, `instanceof` / `in` / `delete`
- Rest / default parameters, spread (array / call / `new` / `super` / object), destructuring (declarations / assignment / parameters / `catch`, with defaults and rest), optional chaining `?.`
- Symbols (including `Symbol.iterator` / `toPrimitive` / `hasInstance`), the iteration protocol, `for-of` (closing the iterator on `break` / `return` / throw) / `for-in`
- Generators (`yield` / `yield*`, `return()` / `throw()` through `finally`), Promises (`then` / `catch` / `finally` / `all` / `race`), `async` / `await`, with the microtask queue drained after the script finishes
- Regex literals and `RegExp` (the regress engine, matching by UTF-16 code unit; `exec` / `test`, `lastIndex` for `g` / `y`, named groups) and the string methods `match` / `matchAll` / `replace` / `replaceAll` / `search` / `split`
- BigInt literals and arithmetic (arbitrary precision; mixing with Number throws TypeError, comparable, `BigInt()` / `toString(radix)`)
- The temporal dead zone (`let` / `const` / `class` and `this` before `super()` in derived classes; checks are dropped at compile time where initialization can be proven), the script scope (top-level `let` / `const` / `class` shared across scripts, not on the global object)

- Class static blocks / private names (fields, methods, accessors, `#x in o`) / computed field names, async generators / `for await`, tagged templates (cached per call site), `new.target`, `arguments`
- Dynamic code: global `Function` / `eval` (indirect), the generator / async function constructors; early errors (oxc_semantic checks plus validation when regex literals are parsed) are reported as SyntaxError at compile time
- Strict mode (directive prologues / class bodies / inherited from the outside): in sloppy calls `this` becomes the global object or a wrapper object; in strict mode failed assignments / deletes throw TypeError, assigning to an undeclared name throws ReferenceError, `arguments.callee` throws TypeError; `eval(…)` in strict code runs as a direct eval (the code is strict, its `var`s stay inside it)

- The property model follows the spec's object internal methods (ValidateAndApplyPropertyDescriptor, array holes and per-element attributes, read-only `length`, non-extensible objects, indexes on string wrapper objects); every method of `Object` / `Array` (Array methods are generic over array-likes, species)
- Date, RegExp's Symbol protocol, String / Number / Math / Symbol / URI functions completed, Promise subclassing and combinators, WeakRef / FinalizationRegistry (strong references)
- ArrayBuffer / SharedArrayBuffer / DataView / TypedArray (including Float16 and lengths that track resizable buffers) / Atomics
- Proxy / Reflect, Iterator helpers, Set methods (union and friends), Map.groupBy
- Host functions distinguish whether they can be `new`ed, and builtin constructors pick the prototype from `new.target` (`class X extends Map` and the like)

Not supported yet (reported as `Error::Unsupported`, with `file:line:column`): top-level await in modules, and the rule about `arguments` conflicting with eval in parameter expressions. The test262 baseline is in `tests/test262.passing`; the plan for the rest is in PLAN.md.

Known deviation: JS re-entered from a host function (the callback of `map`, say) runs to completion within the current slice, and nothing is collected meanwhile (`Function.prototype.call` / `apply` are expanded by the executor into ordinary calls and do not count as re-entry).

## 📜 test262

`tests/test262` is the tc39/test262 submodule (a shallow clone, fetched with `git submodule update --init`); the runner `tests/test262.rs` is a test of the root package (`harness = false`, not published):

```sh
cargo test --release --test test262 -- language/expressions/addition built-ins/Math   # run these directories
J8_TEST262_JIT=1 cargo test --release --test test262                                   # with the JIT on (every function compiled on its first call)
J8_TEST262_VERBOSE=1 cargo test --release --test test262 -- <path>                    # list why each failing test failed
J8_TEST262_UPDATE=1 cargo test --release --test test262 -- <path>                     # write the results back to the baseline
cargo test --release --test test262                                                   # run only the baseline tests, fail if any drops
```

- One subprocess per test with a 10 s timeout; results are pass / fail / unsupported (j8 reported `Error::Unsupported`) / skip / timeout / crash, summarized by directory
- The baseline `tests/test262.passing`: one passing test per line; with no arguments only these run, as a regression check
- Only sloppy mode runs (`onlyStrict` runs strict); `module` is skipped; there is no `$262` host object; line numbers in errors for unsupported syntax include the prepended harness

## ✍️ Conventions


- Submodules are private `mod`s, the crate root flattens exports with `pub use`; ⛔ `pub mod`
- ⛔ `static` and the `'static` lifetime (except `&'static str` names inside enums); background tasks borrow the caller's data with `Task<'t>`
- Strings are turned into enums right after deserialization; ⛔ making decisions on strings
- Conversions go through `From` / `TryFrom`
- Unit tests only pin down facts that are certain (run one at a time, with `timeout 15`)

## 📄 License

MIT.

<div align="center">

**🦀 Same language, written another way.**

[⬆️ Back to top](#top) · [🇨🇳 简体中文](README.zh-CN.md)

</div>
