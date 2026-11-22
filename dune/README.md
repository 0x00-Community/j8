# dune

**English** · [简体中文](README.zh-CN.md)

A JavaScript runtime on [j8](https://github.com/0x00-Community/j8) with two compatibility layers, each in its own
directory:

| Layer | Directory | What it provides |
|---|---|---|
| **Web** | `src/web/` | The Web-standard APIs servers share (WinterTC's minimum common API): `console`, `setTimeout` / `setInterval` / `queueMicrotask`, `structuredClone`, `EventTarget` / `Event` / `CustomEvent`, `AbortController` / `AbortSignal`, `DOMException`, `TextEncoder` / `TextDecoder`, `atob` / `btoa`, `URL` / `URLSearchParams`, `Blob` / `File`, `Headers` / `Request` / `Response` / `fetch` (plain `http:` and `data:`), `performance`, `crypto.getRandomValues` / `randomUUID`, `navigator`, `reportError` |
| **Node** | `src/node/` | Node compatibility on top of the Web layer: `process` (an `EventEmitter`; `argv`, `env`, `exit`, `exitCode`, `nextTick`, `hrtime`, `stdout` / `stderr`, `'exit'` / `'uncaughtException'`), `require` / `module` with Node's resolution (`node_modules`, package.json `main` / `exports`, `.json`), ES modules (`.mjs`, `"type": "module"`; bare imports through `node_modules` and package.json `exports` / `imports`), `Buffer`, `global`, `setImmediate`, and the modules `fs` (+ `fs/promises`, callbacks), `path`, `os`, `events`, `util`, `buffer`, `assert` (+ `assert/strict`), `url`, `timers` (+ `timers/promises`), `child_process` (synchronous), `crypto`, `module`, `string_decoder`, `perf_hooks`, `stream` (+ `stream/promises`, `stream/consumers`), `querystring`, `punycode`, `async_hooks`; `buffer`, `events`, `path` (+ `win32`), `util`, `assert`, `querystring`, `punycode`, `stream` and, on Linux / macOS, `fs` are Node's own |

```sh
cargo run --release -- examples/node.js              # Node layer (the default): CommonJS or ES module by Node's rules
cargo run --release -- --web examples/web.mjs        # Web layer only: files run as ES modules
cargo run --release -- -p '[1, 2].map(x => x * 2)'   # run source and print the result (-e runs without printing)
cargo run --release                                  # REPL (.help / .exit)
cargo run --release -- --jit a.js                    # with the JIT
```

This is a package of its own (not part of the j8 workspace) and depends on the published `j8` crate.

## Install

```sh
curl -fsSL https://j8.app238.com/install | bash               # macOS, Linux: into ~/.dune/bin
powershell -c "irm https://j8.app238.com/install.ps1 | iex"   # Windows
npm install -g @jabberwocky238/dune                            # prebuilt binary for your platform, dune and dunx
cargo install j8dune      # or build it (the name dune is taken on crates.io); then ln -s dune ~/.cargo/bin/dunx
dune upgrade              # replace itself with the latest GitHub release
```

Every `vX.Y.Z` tag (a new major, minor or patch) builds the binaries for all platforms in `npm/platforms.json`, attaches them to a GitHub
release (`dune-<platform>.tar.gz` and `SHA256SUMS`) and publishes the npm packages
with npm trusted publishing (`.github/workflows/dune-release.yml`); the same tag publishes the j8 crates and `j8dune` to
crates.io with the `CRATE_TOKEN` secret (`.github/workflows/j8-release.yml`, crates.io only), and the dune build waits for that j8. The install scripts are served from `mdbook/install/`.

There is one executable. `dunx` is a link to `dune`, which tells the two apart by the name it is started under, as
`bun` / `bunx` do. On npm, `@jabberwocky238/dune` carries placeholders and depends on one package per platform
(`@jabberwocky238/dune-linux-x64`, `@jabberwocky238/dune-linux-x64-musl`, `@jabberwocky238/dune-darwin-arm64`, `@jabberwocky238/dune-win32-x64`, ...); npm installs the one that
fits, and the postinstall script puts its binary in place of the placeholders and links `dunx` to it. `npm/` holds
the main package and `npm/pack.js`, which lays out all the packages of a release from the built binaries.

## Command line

Each folder under `src/cli/` is one level of subcommands: `run/` is `dune run`, `repl/` is `dune repl`, `x/` is
`dune x`. `--jit` and `--web` are accepted before or after the subcommand.

## dunx

`dunx` (the same as `dune x`) runs a package's executable:

```sh
dunx cowsay hello                 # the package's own executable
dunx semver@^7 1.2.3 -r '>1.0.0'  # a version or range; options after the command are the executable's
dunx -p typescript tsc --version  # an executable whose name differs from its package
dunx --no-install json5 --version # never touch the registry
```

The package comes from the nearest `node_modules` up from the current directory, else from the cache
(`$DUNE_CACHE`, default `~/.cache/dune/x`), else it is installed there from the registry (`$DUNE_REGISTRY` /
`$npm_config_registry`, default registry.npmjs.org): dependencies, optional dependencies for this platform and peers
in a hoisted `node_modules`, every tarball checked against its integrity. Lifecycle scripts do not run. JavaScript
executables run in the same process on the Node layer; others (native programs, shell scripts) run as child processes.

## How it is built

Each layer is one `j8::runtime!` declaration, and the Node one stands on the Web one:

```rust
j8::runtime! { pub Web { natives: [Stdio, Clock, Codec, Random, Timer, Http], prelude: PRELUDE, } }
j8::runtime! { pub Dune: Web { globals: [Process], natives: [Fs, Os, Child, V8, Lib, Raw, Uvfs], prelude: PRELUDE, } }
```

- What needs the machine is Rust, bound with `#[j8::object]` / `#[j8::class]` (`web/mod.rs`, `web/timer.rs`,
  `web/http.rs`, `node/mod.rs`, `node/fs.rs`). Blocking work (files, sockets, sleeping) is an `async fn` that runs on
  a thread of its own and settles its promise on the event loop (`host.rs`); nothing touches the GC directly.
- The APIs themselves are JavaScript, one file per family (`web/*.js`, `node/*.js`), run as the preludes.
- Built-in modules are registered with `__j8_native.define(name, factory)` for `require`, and `NodePlatform` serves
  them to `import` (`node:fs` or `fs`, with their named exports).
- Where Node's own library can run as it is, dune uses it: `node/lib/` holds files of Node v25.9.0's `lib/`
  unchanged (MIT, `node/lib/LICENSE`), compiled the way Node compiles its built-in modules (`node/internal.js`,
  with `primordials` and `internalBinding`). What is C++ in Node is in `node/shim/` under the same module ids,
  which win over `lib/`: the bindings `internal/binding/*` (on the natives `v8.rs`, which looks at the engine's
  objects, `bytes.rs`, the byte work of `Buffer`, and `uvfs.rs`, the file system calls, synchronous or on a thread of
  their own) and a few stand-ins such as `internal/bootstrap/realm`. A
  public module found there (`stream`, `stream/promises`, ...) replaces dune's hand-written one. `build.rs` embeds
  both directories.
- The event loop is `j8::Runtime::run_until_idle`: the process ends when no timer or host work is left; the exit
  code is `process.exitCode`. An error nobody caught (in a timer, a callback) prints `Uncaught …` and exits with 1,
  unless `process.on('uncaughtException')` takes it.

## Tests

`tests/node/` runs Node's own test suite against dune, the way Bun does: files from Node v25.9.0's `test/parallel`
and `test/sequential` are copied as they are, with `common/` and the `fixtures/` they read, and a test passes when
`dune test-….js` exits with 0 (Node's own criterion). Only tests that pass are kept, so every one is a regression
test; the patches to `common/` are marked `// dune:`.

```sh
cargo test --release --test node                          # all of them, in parallel, 10 s each
DUNE_NODE_TEST=test-fs- cargo test --release --test node  # the ones whose path contains this
tests/node/fetch.sh test-path-join.js                     # copy one more from Node v25.9.0
```

## Not there yet

Top-level `await` (j8 does not support it yet), named exports when an ES module imports CommonJS (only `default`), unhandled promise rejections (not reported), `https:` in `fetch`,
`fs.watch` / `fs.watchFile`, `fs.cp`, `http` / `net` servers, web streams (`stream/web`), `async_hooks` hooks and `AsyncLocalStorage` across `await` / timers / callbacks (only synchronous calls inside `run()` see the store), asynchronous `child_process.spawn`, `worker_threads`, `zlib`, `crypto` hashes and
ciphers, `Error.stack` (j8 has no stack traces yet).

## License

MIT, as part of [j8](https://github.com/0x00-Community/j8).
