# benchjsc

**English** · [简体中文](README.zh-CN.md)

Runs the same JavaScript benchmarks as [benchv8](../benchv8) (`../benchv8/bench/*.js`) on
[j8](https://github.com/0x00-Community/j8), JavaScriptCore with its JIT off (`JSC_useJIT=false`, LLInt only) and
JavaScriptCore with its JIT, and prints the comparison.

```sh
cd samples/benchjsc
cargo run --release                     # all three engines, comparison table
cargo run --release -- --engine j8      # only one: j8 / jsc / jsc-nojit
cargo run --release -- --runs 10        # runs per benchmark, the shortest is kept (default 5)
```

- JavaScriptCore is WebKitGTK's system library, used through its C API: install the runtime package
  (`libjavascriptcoregtk-6.0-1` on Debian / Ubuntu); the -dev package isn't needed, the library is linked by file name
- Each engine runs in its own child process (JSC reads its `JSC_*` options once per process); timing covers
  compile + execute, each run in a fresh global context
- Every script's completion value is a checksum that must match across the engines (and V8, see benchv8)
- A workspace of its own (excluded from the root workspace)

## License

MIT, as part of [j8](https://github.com/0x00-Community/j8).
