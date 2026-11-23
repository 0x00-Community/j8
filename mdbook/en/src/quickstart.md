# Quick start

## As a library

```sh
cargo add j8                                              # default feature `runtime`: executor + built-ins + standard platform
cargo add j8 --features jit                               # plus the x86-64 JIT (x86-64 Unix and Windows)
cargo add j8 --no-default-features --features compiler    # only JS ⇒ bytecode
```

The smallest evaluation:

```rust,ignore
use j8::builtins::Builtins;
use j8::executor::Executor;
use j8::frame::{CreateParams, Isolate, Source};
use j8::platform::DefaultPlatform;

let platform = DefaultPlatform::default();
let mut isolate = Isolate::new(&platform, CreateParams::default());
let executor = Executor::default();
let cx = isolate.create_context(&Builtins);
let result = executor.eval(&mut isolate, &cx, &Source { text: "1 + 2 * 3", file: "a.js" })?;
```

Real hosts rarely handle isolates directly; they declare a runtime with `j8::runtime!`, see
[Building a host](embedding.md).

### Features

| feature | brings |
|---|---|
| `runtime` (default) | `executor` + `builtins` + `platform` |
| `bytecode` | `j8::bytecode`: bytecode definitions, encoding / decoding |
| `compiler` | `j8::compiler`: JS ⇒ bytecode (includes `bytecode`) |
| `executor` | `j8::executor`: the bytecode executor (includes `compiler`) |
| `builtins` / `platform` | built-in objects / the standard-library platform |
| `jit` | `j8::jit`: the JIT (x86-64 Unix and Windows, includes `executor`) |
| `json` | `j8::Json<T>`: Rust values crossing as JSON-shaped data through serde |
| `napi` | `j8::napi`: Node-API |
| `taskqueue` / `lockfree` | the task queue / lock-free building blocks |
| `full` | all of the above |

## Running JS with dune

```sh
curl -fsSL https://j8.app238.com/install | bash   # or npm i -g @jabberwocky238/dune, see Installing dune
dune app.js                    # Node-compatible layer (default)
dune --web app.mjs             # Web-standard APIs only
dune -p '[1, 2, 3].map(x => x * 2)'
dune --jit bench.js            # with the JIT
dune                           # REPL
dunx cowsay hello              # run an npm package's executable
```

See [dune and dunx](dune.md).

## From source

```sh
git clone https://github.com/0x00-Community/j8 && cd j8
cargo build --release
git submodule update --init                       # fetch test262
cargo test --release --test test262               # the regression baseline
cd dune && cargo run --release -- examples/node.js
cd samples/j8bcdbg && cargo run -- compile -e 'let s = 0; for (let i = 0; i < 10; i++) s += i; s'
```

`dune/` and `samples/*` are packages of their own, outside the workspace, and depend on the published `j8` by version,
as an embedder would.
