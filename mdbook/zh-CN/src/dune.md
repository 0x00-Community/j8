# dune 与 dunx

`dune/` 是建在 j8 上的 JavaScript 运行时，**对标 Node 和 Bun**；`dunx`（等同 `dune x`）跑 npm 包里的可执行文件，
**对标 npx / bunx**。它自成一包、不在 j8 的 workspace 里，像嵌入方一样依赖 crates.io 上发布的 `j8`——它本身就是
[嵌入 j8](embedding.md) 的完整范例。

## 安装

```sh
cargo install j8dune              # crates.io 上 dune 已被占用；然后 ln -s dune ~/.cargo/bin/dunx
```

npm 渠道照 Bun 的做法准备好了（`dune/npm/`）：主包 `@dune/dune` 只带占位脚本，用 `optionalDependencies` 依赖每个平台一个包
（`@dune/linux-x64`、`@dune/linux-x64-musl`、`@dune/darwin-arm64`、`@dune/win32-x64` ……）；npm 只装符合本机的那个，
postinstall 脚本把其中的二进制硬链接到占位脚本的位置、再把 `dunx` 链过去，所以 `dune` 启动时不经过 Node。
`--ignore-scripts` 时占位脚本经 Node 启动二进制。`npm/pack.js` 用编好的二进制排出一次发版的全部包。

## 一个二进制

只有一个可执行文件。`dunx` 是 `dune` 的链接，程序按启动时的文件名（`dunx` / `dunx.exe`）决定按哪套命令行解析，
和 `bun` / `bunx` 一样。

## 命令行

```sh
dune app.js [args]           # Node 层（缺省）：按 Node 规则当 CommonJS 或 ES 模块跑
dune run app.js [args]       # 同上
dune --web app.mjs           # 只要 Web 层：文件当 ES 模块跑
dune -e 'code'               # 跑源码
dune -p 'code'               # 跑源码并打印结果
dune - < app.js              # 从标准输入读
dune                         # REPL（.help / .exit）
dune repl
dune --jit app.js            # 打开 JIT
dune x <pkg> [args]          # 等同 dunx
```

`dune/src/cli/` 下**每一层文件夹就是一层子命令**：`run/` 是 `dune run`，`repl/` 是 `dune repl`，`x/` 是 `dune x`
（包安装器也在 `x/` 里）。`--jit`、`--web` 写在子命令前后都行；脚本路径之后的参数全部原样交给脚本
（`dune a.js run x` 把 `run x` 给 a.js）。

## 两层

每层是一个 `j8::runtime!` 声明，Node 层叠在 Web 层上：

```rust,ignore
j8::runtime! { pub Web { natives: [Stdio, Clock, Codec, Random, Timer, Http], prelude: PRELUDE, } }
j8::runtime! { pub Dune: Web { globals: [Process], natives: [Fs, Os, Child], prelude: PRELUDE, } }
```

| 层 | 目录 | 提供什么 |
|---|---|---|
| **Web** | `src/web/` | WinterTC 最小公共 API：`console`、`setTimeout` / `setInterval` / `queueMicrotask`、`structuredClone`、`EventTarget` / `Event` / `CustomEvent`、`AbortController` / `AbortSignal`、`DOMException`、`TextEncoder` / `TextDecoder`、`atob` / `btoa`、`URL` / `URLSearchParams`、`Blob` / `File`、`Headers` / `Request` / `Response` / `fetch`（明文 `http:` 与 `data:`）、`performance`、`crypto.getRandomValues` / `randomUUID`、`navigator`、`reportError` |
| **Node** | `src/node/` | `process`（是 `EventEmitter`；`argv`、`env`、`exit`、`exitCode`、`nextTick`、`hrtime`、`stdout` / `stderr`）、按 Node 规则解析的 `require` / `module`、ES 模块、`Buffer`、`global`、`setImmediate`，以及 `fs`（含 `fs/promises`）、`path`、`os`、`events`、`util`、`buffer`、`assert`、`url`、`timers`、`child_process`（同步）、`crypto`、`module`、`string_decoder`、`perf_hooks` |

搭法：

- 碰机器的部分写在 Rust 里，用 `#[j8::object]` / `#[j8::class]` 绑定（`web/mod.rs`、`web/timer.rs`、`web/http.rs`、
  `node/mod.rs`、`node/fs.rs`）。会阻塞的活（文件、套接字、睡眠）写成 `async fn`，在单独的线程上跑，到事件循环里兑现
  promise（`host.rs`）。没有任何地方直接碰 GC。
- 接口本身是 JavaScript，一族一个文件（`web/*.js`、`node/*.js`），作为序言运行。
- 内置模块用 `__j8_native.define(name, factory)` 登记给 `require`，`NodePlatform` 把它们供给 `import`（`node:fs` 或 `fs`，
  带具名导出）。
- 事件循环是 `j8::Runtime::run_until_idle`：没有定时器和宿主任务时进程结束，退出码取 `process.exitCode`。没人接住的错误
  打印 `Uncaught …` 并以 1 退出，除非 `process.on('uncaughtException')` 接走了。

## dunx

```sh
dunx cowsay hello                   # 包自己的可执行文件
dunx semver@^7 1.2.3 -r '>1.0.0'    # 指定版本或范围；命令之后的选项都给可执行文件
dunx -p typescript tsc --version    # 可执行文件名和包名不同
dunx --no-install json5 --version   # 不访问 registry
```

查找顺序：

1. 从当前目录往上找最近的 `node_modules`（没指定版本时先看 `.bin/<命令>`）；
2. 缓存（`$DUNE_CACHE`，缺省 `~/.cache/dune/x`）；
3. 都没有就从 registry（`$DUNE_REGISTRY` / `$npm_config_registry`，缺省 registry.npmjs.org）装进缓存。

安装器（`src/cli/x/`：`spec.rs` 解析说明符、`registry.rs` 取元数据、`tree.rs` 解依赖树、`manifest.rs` 读 package.json）
把依赖、适合本平台的可选依赖和 peer 依赖装成**提升式**的 `node_modules`，每个 tarball 都校验 integrity，**不跑生命周期脚本**。
JavaScript 可执行文件（按 `#!` 行或扩展名判断）在本进程的 Node 层里跑，其余（原生程序、shell 脚本）作为子进程跑。

实测能跑：cowsay、semver、json5、esbuild、biome、tsc 7。

## 还没有的

顶层 `await`、ES 模块 import CommonJS 时的具名导出（只有 `default`）、未处理的 promise 拒绝的报告、`fetch` 的 `https:`、
`http` / `net` 服务端、流、异步的 `child_process.spawn`、`worker_threads`、`zlib`、`crypto` 的哈希与加解密、`Error.stack`。
