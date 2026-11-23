# j8framework

**English** · [简体中文](README.zh-CN.md)

The core framework layer of [j8](https://github.com/0x00-Community/j8). It defines the traits every other layer
implements and the structs / enums they share; it depends on no other j8 layer.

## What is in it

| Item | Role |
| --- | --- |
| `Platform` / `Pool` | Threads and clocks lent by the embedder (`j8platform` implements them) |
| `Isolate` | An independent JS world: a GC heap ([paragc](https://crates.io/crates/paragc)) plus a profiler |
| `Scope` | One entry into an isolate with a `Context` and the executor; values in it carry the `'scope` brand and cannot leave it; all JS operation semantics hang off it, calls back into JS go through `Invoke` |
| `ScopeGlobal<'p, B>` | Roots that live across `enter` calls; `B` names the type (`ValueBrand`, `ContextBrand`) |
| `Context` | An execution context: the global object and the built-in prototypes (`Intrinsics`, implemented by `j8builtins`) |
| `Host` / `HostObject` | A Rust value inside a JS object, traced by the collector |
| `Value` / `JsObject` / `JsString` … | NaN-boxed values and heap objects |

### The GC layer

The heap is paragc, used only through its public API. `gc.rs` adds what a JS engine needs on top:

- `GcCell`: a lock-free `RefCell` whose data sits in a snapshot behind a paragc `Slot` (young snapshots are changed in
  place, older ones are moved into a fresh snapshot first, so the write barrier sees it)
- `ValueCell`: one NaN-boxed `Value` (an atomic word plus a `Slot`)
- `Cow`, `GcQueue`, `Static`, and `#[derive(Collect)]` (from `j8macros`)

Objects keep what hot paths touch outside their cell: prototype, properties (a shape plus a value store in the GC heap,
see `shape.rs` / `store.rs`), array elements, and the immutable data of functions and regexes. Rules: write a slot only
once its owner is on the heap; heap types are `Send + Sync`; the heap collects stop-the-world only.

## Usage

```rust
use j8builtins::Builtins;
use j8executor::Executor;
use j8framework::{CreateParams, Isolate, ContextBrand, Source};
use j8platform::DefaultPlatform;

let platform = DefaultPlatform::default();
let mut isolate = Isolate::new(&platform, CreateParams::default());
let executor = Executor::default();
let cx = isolate.create_context(&Builtins);
let result = executor.eval(&mut isolate, &cx, &Source { text: "1 + 2 * 3", file: "a.js" })?;
```

## License

MIT, as part of [j8](https://github.com/0x00-Community/j8).
