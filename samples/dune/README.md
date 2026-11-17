# dune

**English** · [简体中文](README.zh-CN.md)

A node-style JavaScript runtime built on [j8](https://github.com/0x00-Community/j8) (a sample host; comparable to
node / deno).

```sh
cargo run --release -p dune -- a.js               # run a file (`dune run a.js`, `-` reads standard input)
cargo run --release -p dune -- -e '<js>'          # run source
cargo run --release -p dune -- -p '[1, 2].map(x => x * 2)'   # run and print the result
cargo run --release -p dune                       # REPL (.help / .exit)
cargo run --release -p dune -- --jit a.js         # with the JIT
```

- `require` / `module` and the native parts of `fs` / `path` / `process` are bound in Rust with `#[j8::object]`
  (`src/node.rs`); the rest is JavaScript in `src/prelude.js`
- An uncaught exception prints `Uncaught …` and exits with code 1
- The test262 runner (`tests/test262.rs` at the repository root) is this package's test:
  `cargo test --release -p dune --test test262`

## License

MIT, as part of [j8](https://github.com/0x00-Community/j8).
