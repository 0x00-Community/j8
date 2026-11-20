# benchquickjs

**English** · [简体中文](README.zh-CN.md)

Runs the same JavaScript benchmarks as [benchv8](../benchv8) (`../benchv8/bench/*.js`) on
[j8](https://github.com/0x00-Community/j8) and [QuickJS](https://github.com/quickjs-ng/quickjs) (quickjs-ng, built from
source by [rquickjs](https://crates.io/crates/rquickjs)), and prints the comparison. QuickJS has no JIT, so this is
interpreter against interpreter.

```sh
cd samples/benchquickjs
cargo run --release                         # both engines, comparison table
cargo run --release -- --engine quickjs     # only one: j8 / quickjs
cargo run --release -- --runs 10            # runs per benchmark, the shortest is kept (default 5)
```

- Each engine runs in its own child process; timing covers compile + execute, each run in a fresh context
- Every script's completion value is a checksum that must match across the engines (and V8 / JavaScriptCore, see
  benchv8 / benchjsc)
- A workspace of its own (excluded from the root workspace); building QuickJS needs a C compiler

## License

MIT, as part of [j8](https://github.com/0x00-Community/j8).
