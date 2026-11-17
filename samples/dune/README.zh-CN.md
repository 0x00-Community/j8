# dune

[English](README.md) · **简体中文**

基于 [j8](https://github.com/0x00-Community/j8) 的 node 式 JavaScript 运行时（样例宿主，对标 node / deno）。

```sh
cargo run --release -p dune -- a.js               # 跑文件（`dune run a.js`，`-` 读标准输入）
cargo run --release -p dune -- -e '<js>'          # 跑源码
cargo run --release -p dune -- -p '[1, 2].map(x => x * 2)'   # 跑并打印结果
cargo run --release -p dune                       # REPL（.help / .exit）
cargo run --release -p dune -- --jit a.js         # 打开 JIT
```

- `require` / `module` 和 `fs` / `path` / `process` 的原生部分用 `#[j8::object]` 在 Rust 里绑定（`src/node.rs`），其余是
  `src/prelude.js` 里的 JavaScript
- 没接住的异常打印 `Uncaught …`，退出码 1
- test262 跑测器（仓库根的 `tests/test262.rs`）是本包的测试：`cargo test --release -p dune --test test262`

## 许可

MIT，随 [j8](https://github.com/0x00-Community/j8) 一起。
