# dune and dunx

`dune/` is a JavaScript runtime built on j8, **on par with Node and Bun**; `dunx` (the same as `dune x`) runs the
executables in npm packages, **like npx / bunx**. It is a package of its own, outside j8's workspace, and depends on the
published `j8` as an embedder would: it is itself the complete example of [embedding j8](embedding.md).

## Install

```sh
curl -fsSL https://j8.app238.com/install | bash                 # macOS, Linux
powershell -c "irm https://j8.app238.com/install.ps1 | iex"     # Windows
npm install -g @jabberwocky238/dune
cargo install j8dune              # the name dune is taken on crates.io; then ln -s dune ~/.cargo/bin/dunx
dune upgrade                      # replace itself with the latest GitHub release
```

See [Installing dune](install.md) for every option. The npm channel works the way Bun's does (`dune/npm/`): the main package `@jabberwocky238/dune` carries placeholders and
depends, through `optionalDependencies`, on one package per platform (`@jabberwocky238/dune-linux-x64`, `@jabberwocky238/dune-linux-x64-musl`,
`@jabberwocky238/dune-darwin-arm64`, `@jabberwocky238/dune-win32-x64` …). npm installs only the one that fits, and the postinstall script hard-links its
binary over the placeholders and links `dunx` to it, so `dune` starts without Node. With `--ignore-scripts` the
placeholders start the binary through Node. `npm/pack.js` lays out every package of a release from the built binaries.

## One executable

There is one executable. `dunx` is a link to `dune`, which picks its command line by the name it was started under
(`dunx` / `dunx.exe`), as `bun` / `bunx` do.

## Command line

```sh
dune app.js [args]           # Node layer (default): CommonJS or an ES module, by Node's rules
dune run app.js [args]       # the same
dune --web app.mjs           # Web layer only: files run as ES modules
dune -e 'code'               # run source
dune -p 'code'               # run source and print the result
dune - < app.js              # read stdin
dune                         # REPL (.help / .exit)
dune repl
dune --jit app.js            # with the JIT
dune x <pkg> [args]          # the same as dunx
```

**Each folder under `dune/src/cli/` is one level of subcommands**: `run/` is `dune run`, `repl/` is `dune repl`, `x/` is
`dune x` (the package installer lives in `x/` too). `--jit` and `--web` go before or after the subcommand; everything
after the script path goes to the script as is (`dune a.js run x` gives `run x` to a.js).

## Two layers

Each layer is one `j8::runtime!` declaration, Node on top of Web:

```rust,ignore
j8::runtime! { pub Web { natives: [Stdio, Clock, Codec, Random, Timer, Http], prelude: PRELUDE, } }
j8::runtime! { pub Dune: Web { globals: [Process], natives: [Fs, Os, Child], prelude: PRELUDE, } }
```

| Layer | Directory | Provides |
|---|---|---|
| **Web** | `src/web/` | the WinterTC minimum common API: `console`, `setTimeout` / `setInterval` / `queueMicrotask`, `structuredClone`, `EventTarget` / `Event` / `CustomEvent`, `AbortController` / `AbortSignal`, `DOMException`, `TextEncoder` / `TextDecoder`, `atob` / `btoa`, `URL` / `URLSearchParams`, `Blob` / `File`, `Headers` / `Request` / `Response` / `fetch` (plain `http:` and `data:`), `performance`, `crypto.getRandomValues` / `randomUUID`, `navigator`, `reportError` |
| **Node** | `src/node/` | `process` (an `EventEmitter`; `argv`, `env`, `exit`, `exitCode`, `nextTick`, `hrtime`, `stdout` / `stderr`), `require` / `module` resolved by Node's rules, ES modules, `Buffer`, `global`, `setImmediate`, and the modules `fs` (with `fs/promises`), `path`, `os`, `events`, `util`, `buffer`, `assert`, `url`, `timers`, `child_process` (sync), `crypto`, `module`, `string_decoder`, `perf_hooks` |

How it is built:

- What needs the machine is Rust, bound with `#[j8::object]` / `#[j8::class]` (`web/mod.rs`, `web/timer.rs`,
  `web/http.rs`, `node/mod.rs`, `node/fs.rs`). Blocking work (files, sockets, sleeping) is `async fn`s that run on threads
  of their own and settle promises on the event loop (`host.rs`). Nothing touches the GC directly.
- The APIs themselves are JavaScript, one file per family (`web/*.js`, `node/*.js`), run as the prelude.
- Built-in modules register with `require` through `__j8_native.define(name, factory)`; `NodePlatform` serves them to
  `import` (`node:fs` or `fs`, with named exports).
- The event loop is `j8::Runtime::run_until_idle`: the process ends when no timers and no host work are left, with
  `process.exitCode`. An uncaught error prints `Uncaught …` and exits with 1, unless `process.on('uncaughtException')`
  takes it.

## dunx

```sh
dunx cowsay hello                   # the package's own executable
dunx semver@^7 1.2.3 -r '>1.0.0'    # a version or range; options after the command are the executable's
dunx -p typescript tsc --version    # an executable whose name differs from its package
dunx --no-install json5 --version   # never touch the registry
```

Lookup order:

1. the nearest `node_modules` up from the current directory (`.bin/<command>` first when no version is asked for);
2. the cache (`$DUNE_CACHE`, default `~/.cache/dune/x`);
3. otherwise, install from the registry (`$DUNE_REGISTRY` / `$npm_config_registry`, default registry.npmjs.org) into the
   cache.

The installer (`src/cli/x/`: `spec.rs` parses specifiers, `registry.rs` fetches metadata, `tree.rs` resolves the tree,
`manifest.rs` reads package.json) installs dependencies, optional dependencies for this platform and peers into a
**hoisted** `node_modules`, checks every tarball against its integrity, and **runs no lifecycle scripts**. JavaScript
executables (by the `#!` line or the extension) run in this process on the Node layer; others (native programs, shell
scripts) run as child processes.

Known to work: cowsay, semver, json5, esbuild, biome, tsc 7.

## Not yet

Top-level `await`, named exports when an ES module imports CommonJS (only `default`), reporting unhandled rejections,
`https:` in `fetch`, `http` / `net` servers, streams, async `child_process.spawn`, `worker_threads`, `zlib`, hashing and
ciphers in `crypto`, `Error.stack`.
