# dune

[English](README.md) · **简体中文**

基于 [j8](https://github.com/0x00-Community/j8) 的 JavaScript 运行时，分两层兼容，各占一个目录：

| 层 | 目录 | 提供什么 |
|---|---|---|
| **Web** | `src/web/` | 服务端共有的 Web 标准接口（WinterTC 最小公共 API）：`console`、`setTimeout` / `setInterval` / `queueMicrotask`、`structuredClone`、`EventTarget` / `Event` / `CustomEvent`、`AbortController` / `AbortSignal`、`DOMException`、`TextEncoder` / `TextDecoder`、`atob` / `btoa`、`URL` / `URLSearchParams`、`Blob` / `File`、`Headers` / `Request` / `Response` / `fetch`（明文 `http:` 与 `data:`）、`performance`、`crypto.getRandomValues` / `randomUUID`、`navigator`、`reportError` |
| **Node** | `src/node/` | 叠在 Web 层上的 Node 兼容：`process`（是 `EventEmitter`；`argv`、`env`、`exit`、`exitCode`、`nextTick`、`hrtime`、`stdout` / `stderr`、`'exit'` / `'uncaughtException'`）、按 Node 规则解析的 `require` / `module`（`node_modules`、package.json 的 `main` / `exports`、`.json`）、ES 模块（`.mjs`、`"type": "module"`；裸说明符按 `node_modules` 与 package.json 的 `exports` / `imports` 解析）、`Buffer`、`global`、`setImmediate`，以及模块 `fs`（含 `fs/promises`、回调式）、`path`、`os`、`events`、`util`、`buffer`、`assert`（含 `assert/strict`）、`url`、`timers`（含 `timers/promises`）、`child_process`（同步）、`crypto`、`module`、`string_decoder`、`perf_hooks`、`stream`（含 `stream/promises`、`stream/consumers`）、`querystring`、`punycode`、`async_hooks`；其中 `buffer`、`events`、`path`（含 `win32`）、`util`、`assert`、`querystring`、`punycode`、`stream` 以及 Linux / macOS 上的 `fs` 用的是 Node 自己的实现 |

```sh
cargo run --release -- examples/node.js              # Node 层（缺省）：按 Node 规则当 CommonJS 或 ES 模块跑
cargo run --release -- --web examples/web.mjs        # 只要 Web 层：文件当 ES 模块跑
cargo run --release -- -p '[1, 2].map(x => x * 2)'   # 跑源码并打印结果（-e 只跑不打印）
cargo run --release                                  # REPL（.help / .exit）
cargo run --release -- --jit a.js                    # 打开 JIT
```

本包自成一体（不在 j8 的 workspace 里），依赖 crates.io 上发布的 `j8`。

## 安装

```sh
curl -fsSL https://j8.app238.com/install | bash               # macOS、Linux：装进 ~/.dune/bin
powershell -c "irm https://j8.app238.com/install.ps1 | iex"   # Windows
npm install -g @jabberwocky238/dune                            # 本平台的预编译二进制，dune 和 dunx 都有
cargo install j8dune      # 或者自己编（crates.io 上 dune 已被占用）；再 ln -s dune ~/.cargo/bin/dunx
dune upgrade              # 用 GitHub 上的最新 Release 替换自己
```

每个 `vX.Y.Z` 标签（主、次、补丁任一位变化）会编出 `npm/platforms.json` 里全部平台的二进制，挂到 GitHub Release（`dune-<平台>.tar.gz`
和 `SHA256SUMS`），并用 npm trusted publishing 发布 npm 包（`.github/workflows/dune-release.yml`）；同一标签由 `.github/workflows/j8-release.yml`
把 j8 各 crate 和 `j8dune` 发到 crates.io（它只管 crates.io），dune 的构建会等这一版 j8 上线。安装脚本放在 `mdbook/install/`。

只有一个可执行文件：`dunx` 是 `dune` 的链接，程序按启动时的文件名区分两者（同 `bun` / `bunx`）。npm 上的 `@jabberwocky238/dune`
只带占位脚本，依赖每个平台一个包（`@jabberwocky238/dune-linux-x64`、`@jabberwocky238/dune-linux-x64-musl`、`@jabberwocky238/dune-darwin-arm64`、`@jabberwocky238/dune-win32-x64`
……），npm 只装符合本机的那个；postinstall 脚本把其中的二进制放到占位脚本的位置，再把 `dunx` 链接过去。`npm/` 放主包和
`npm/pack.js`，后者用编好的二进制排出一次发版的全部包。

## 命令行

`src/cli/` 下每一层文件夹就是一层子命令：`run/` 是 `dune run`，`repl/` 是 `dune repl`，`x/` 是 `dune x`。`--jit`、`--web`
写在子命令前后都行。

## dunx

`dunx`（等同 `dune x`）用来跑包里的可执行文件：

```sh
dunx cowsay hello                 # 包自己的可执行文件
dunx semver@^7 1.2.3 -r '>1.0.0'  # 指定版本或范围；命令之后的选项都给可执行文件
dunx -p typescript tsc --version  # 可执行文件名和包名不同
dunx --no-install json5 --version # 不访问 registry
```

先从当前目录往上找最近的 `node_modules`，再找缓存（`$DUNE_CACHE`，缺省 `~/.cache/dune/x`），都没有就从 registry
（`$DUNE_REGISTRY` / `$npm_config_registry`，缺省 registry.npmjs.org）装进缓存：依赖、适合本平台的可选依赖和 peer
依赖装成提升式的 `node_modules`，每个 tarball 都校验 integrity，不跑生命周期脚本。JavaScript 可执行文件在本进程的
Node 层里跑，其余（原生程序、shell 脚本）作为子进程跑。

## 怎么搭的

每层是一个 `j8::runtime!` 声明，Node 层叠在 Web 层上：

```rust
j8::runtime! { pub Web { natives: [Stdio, Clock, Codec, Random, Timer, Http], prelude: PRELUDE, } }
j8::runtime! { pub Dune: Web { globals: [Process], natives: [Fs, Os, Child, V8, Lib, Raw, Uvfs], prelude: PRELUDE, } }
```

- 要碰机器的部分写在 Rust 里，用 `#[j8::object]` / `#[j8::class]` 绑定（`web/mod.rs`、`web/timer.rs`、`web/http.rs`、
  `node/mod.rs`、`node/fs.rs`）。会阻塞的活（文件、套接字、睡眠）写成 `async fn`，在单独的线程上跑，到事件循环里
  兑现 promise（`host.rs`）；没有任何地方直接碰 GC。
- 接口本身是 JavaScript，一族一个文件（`web/*.js`、`node/*.js`），作为 prelude 运行。
- 内置模块用 `__j8_native.define(name, factory)` 登记给 `require`，`NodePlatform` 把它们供给 `import`（`node:fs` 或
  `fs`，带具名导出）。
- Node 自己的库能原样跑的地方就用它：`node/lib/` 放 Node v25.9.0 `lib/` 的原文件（MIT，`node/lib/LICENSE`），按 Node
  编译内置模块的方式编译（`node/internal.js`，带 `primordials` 和 `internalBinding`）。Node 里是 C++ 的部分放在
  `node/shim/`，用同样的模块 id，优先于 `lib/`：binding `internal/binding/*`（建在原生对象 `v8.rs` 上——它看引擎里的
  对象——`bytes.rs`——`Buffer` 的字节活——和 `uvfs.rs`——文件系统调用，同步或在单独线程上）和少量替身，如 `internal/bootstrap/realm`。在那里找到的公开模块
  （`stream`、`stream/promises` 等）取代 dune 手写的同名模块。`build.rs` 把两个目录嵌进二进制。
- 事件循环是 `j8::Runtime::run_until_idle`：没有定时器和宿主任务时进程结束，退出码取 `process.exitCode`。没人接住的
  错误（定时器里、回调里）打印 `Uncaught …` 并以 1 退出，除非 `process.on('uncaughtException')` 接走了。

## 测试

`tests/node/` 照 Bun 的做法拿 Node 自己的测试集测 dune：Node v25.9.0 `test/parallel`、`test/sequential` 的文件原样复制，
连同 `common/` 和它们读的 `fixtures/`；`dune test-….js` 退出码为 0 即通过（Node 自己的判据）。只收能过的，
每一个都是回归测试；`common/` 的改动标了 `// dune:`。

```sh
cargo test --release --test node                          # 全部，并行，每个 10 秒
DUNE_NODE_TEST=test-fs- cargo test --release --test node  # 路径含这段的
tests/node/fetch.sh test-path-join.js                     # 再从 Node v25.9.0 拷一个
```

## 还没有的

顶层 `await`（j8 还不支持）、ES 模块 import CommonJS 时的具名导出（只有 `default`）、未处理的 promise 拒绝（不报）、`fetch` 的 `https:`、`fs.watch` / `fs.watchFile`、`fs.cp`、`http` / `net` 服务端、Web 流（`stream/web`）、`async_hooks` 的钩子和跨 `await` / 定时器 / 回调的 `AsyncLocalStorage`（只有 `run()` 里同步调用的代码看得到 store）、异步的
`child_process.spawn`、`worker_threads`、`zlib`、`crypto` 的哈希与加解密、`Error.stack`（j8 还没有调用栈）。

## 许可

MIT，随 [j8](https://github.com/0x00-Community/j8) 一起。
