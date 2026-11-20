# dune

[English](README.md) · **简体中文**

基于 [j8](https://github.com/0x00-Community/j8) 的 JavaScript 运行时，分两层兼容，各占一个目录：

| 层 | 目录 | 提供什么 |
|---|---|---|
| **Web** | `src/web/` | 服务端共有的 Web 标准接口（WinterTC 最小公共 API）：`console`、`setTimeout` / `setInterval` / `queueMicrotask`、`structuredClone`、`EventTarget` / `Event` / `CustomEvent`、`AbortController` / `AbortSignal`、`DOMException`、`TextEncoder` / `TextDecoder`、`atob` / `btoa`、`URL` / `URLSearchParams`、`Blob` / `File`、`Headers` / `Request` / `Response` / `fetch`（明文 `http:` 与 `data:`）、`performance`、`crypto.getRandomValues` / `randomUUID`、`navigator`、`reportError` |
| **Node** | `src/node/` | 叠在 Web 层上的 Node 兼容：`process`（是 `EventEmitter`；`argv`、`env`、`exit`、`exitCode`、`nextTick`、`hrtime`、`stdout` / `stderr`、`'exit'` / `'uncaughtException'`）、按 Node 规则解析的 `require` / `module`（`node_modules`、package.json 的 `main` / `exports`、`.json`）、ES 模块（`.mjs`、`"type": "module"`）、`Buffer`、`global`、`setImmediate`，以及模块 `fs`（含 `fs/promises`、回调式）、`path`、`os`、`events`、`util`、`buffer`、`assert`（含 `assert/strict`）、`url`、`timers`（含 `timers/promises`）、`child_process`（同步）、`crypto`、`module`、`string_decoder`、`perf_hooks` |

```sh
cargo run --release -- examples/node.js              # Node 层（缺省）：按 Node 规则当 CommonJS 或 ES 模块跑
cargo run --release -- --web examples/web.mjs        # 只要 Web 层：文件当 ES 模块跑
cargo run --release -- -p '[1, 2].map(x => x * 2)'   # 跑源码并打印结果（-e 只跑不打印）
cargo run --release                                  # REPL（.help / .exit）
cargo run --release -- --jit a.js                    # 打开 JIT
```

本包自成一体（不在 j8 的 workspace 里），依赖 crates.io 上发布的 `j8`。

## 怎么搭的

每层是一个 `j8::runtime!` 声明，Node 层叠在 Web 层上：

```rust
j8::runtime! { pub Web { natives: [Stdio, Clock, Codec, Random, Timer, Http], prelude: PRELUDE, } }
j8::runtime! { pub Dune: Web { globals: [Process], natives: [Fs, Os, Child], prelude: PRELUDE, } }
```

- 要碰机器的部分写在 Rust 里，用 `#[j8::object]` / `#[j8::class]` 绑定（`web/mod.rs`、`web/timer.rs`、`web/http.rs`、
  `node/mod.rs`、`node/fs.rs`）。会阻塞的活（文件、套接字、睡眠）写成 `async fn`，在单独的线程上跑，到事件循环里
  兑现 promise（`host.rs`）；没有任何地方直接碰 GC。
- 接口本身是 JavaScript，一族一个文件（`web/*.js`、`node/*.js`），作为 prelude 运行。
- 内置模块用 `__j8_native.define(name, factory)` 登记给 `require`，`NodePlatform` 把它们供给 `import`（`node:fs` 或
  `fs`，带具名导出）。
- 事件循环是 `j8::Runtime::run_until_idle`：没有定时器和宿主任务时进程结束，退出码取 `process.exitCode`。没人接住的
  错误（定时器里、回调里）打印 `Uncaught …` 并以 1 退出，除非 `process.on('uncaughtException')` 接走了。

## 还没有的

顶层 `await`（j8 还不支持）、未处理的 promise 拒绝（不报）、`fetch` 的 `https:`、`http` / `net` 服务端、流、异步的
`child_process.spawn`、`worker_threads`、`zlib`、`crypto` 的哈希与加解密、`Error.stack`（j8 还没有调用栈）。

## 许可

MIT，随 [j8](https://github.com/0x00-Community/j8) 一起。
