# Debugging and benchmark tools

## j8bcdbg: reading bytecode

`samples/j8bcdbg` compiles JS and lists the bytecode per function in the format of V8's `--print-bytecode` (example in
[Bytecode](bytecode.md#real-bytecode)).

```sh
cd samples/j8bcdbg
cargo run -- compile a.js                 # `-` reads stdin
cargo run -- compile -e 'let s = 0; for (let i = 0; i < 10; i++) s += i; s'
cargo run -- compile a.js -o a.txt        # write to a file
```

## JIT switches

| Variable | Effect |
|---|---|
| `J8_JIT_LOG=1` | prints every compilation (instructions, bytes, time) and every deopt (reason, place) |
| `J8_JIT_IR=1` | prints the compiled graph |
| `J8_JIT_BIN=<dir>` | writes each piece of machine code to `f<function>-<entry>.bin`, for objdump |
| `J8_NOJIT=1` | interpreter only, in the benchmark samples |

## Three benchmark suites

The same JS benchmarks (`samples/benchv8/bench/*.js`, 11 of them) run on several engines; every script's completion
value is a checksum that must match on all of them:

| Sample | Compared with | Notes |
|---|---|---|
| `samples/benchv8` | V8 (JIT) and V8 `--jitless` | through rusty_v8 152.2.0; downloads V8's prebuilt library |
| `samples/benchjsc` | JavaScriptCore (JIT) and `JSC_useJIT=false` | the system's WebKitGTK library (C API) |
| `samples/benchquickjs` | QuickJS (quickjs-ng 0.16.2) | built from source by rquickjs; QuickJS has no JIT |

```sh
cd samples/benchv8
cargo run --release                     # every engine once, comparison table
cargo run --release -- --engine j8      # only one
cargo run --release -- --runs 10        # runs per benchmark, the shortest kept (default 5)
```

- each engine runs in its own child process (`--jitless` is process-wide);
- timing covers compile + execute, each run in a fresh environment, the shortest kept;
- each is a workspace of its own, so a plain `cargo build --workspace` doesn't pull V8's prebuilt library.

Results and analysis: [Performance](performance.md).
