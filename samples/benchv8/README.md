# benchv8

**English** · [简体中文](README.zh-CN.md)

Runs the same JavaScript benchmarks (`bench/*.js`) on [j8](https://github.com/0x00-Community/j8), V8 `--jitless`
and V8 with its JIT (via rusty_v8), and prints the comparison. Results and analysis: [benchmark.md](benchmark.md).

```sh
cd samples/benchv8
cargo run --release                     # all three engines, comparison table
cargo run --release -- --engine j8      # only one: j8 / v8 / v8-jitless
cargo run --release -- --runs 10        # runs per benchmark, the shortest is kept (default 5)
```

- Each engine runs in its own child process (`--jitless` is process-wide); timing covers compile + execute, each run in
  a fresh execution environment
- Every script's completion value is a checksum that must match across the engines
- A workspace of its own (excluded from the root workspace): it downloads V8's prebuilt library

## License

MIT, as part of [j8](https://github.com/0x00-Community/j8).
