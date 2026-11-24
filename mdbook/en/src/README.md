# Preface

This book explains [j8](https://github.com/0x00-Community/j8) from end to end: a JavaScript engine written from scratch
in Rust, layered like V8. JS is compiled to bytecode that matches V8's Ignition byte for byte, interpreted by the
executor, and hot functions and loops go to a speculative JIT that emits x86-64 or AArch64 machine code. The heap is
[paragc](https://crates.io/crates/paragc).

## Install dune

```sh
curl -fsSL https://j8.app238.com/install | bash                 # macOS, Linux
powershell -c "irm https://j8.app238.com/install.ps1 | iex"     # Windows
npm install -g @litany/dune                              # anywhere with npm
```

More in [Installing dune](install.md).

## How to read it

| You want to … | Start here |
|---|---|
| see it run | [Quick start](quickstart.md) |
| understand the design | [Architecture](architecture.md), then "Inside the engine" in order |
| build your own runtime on j8 | the "Embedding j8" part, from [Building a host](embedding.md) |
| run scripts and npm packages with dune | [dune and dunx](dune.md) |
| contribute | [Code conventions](conventions.md), [test262 and testing](testing.md) |

## Conventions

- Paths are relative to the j8 repository root, e.g. `crates/j8executor/src/ic.rs`.
- "V8" means the V8 engine itself; j8 tracks V8 15.2.124, the version inside rusty_v8 152.2.0.
- The book describes the repository's **main branch** (November 2026). j8 0.4.0 on crates.io predates the V8 bytecode
  and the speculative JIT; the next release carries them.
- Numbers (test262 passes, benchmark times) are as of writing; the repository has the current ones.

The book's source is the repository's `mdbook/` directory, in English (`en/`) and Chinese (`zh-CN/`). Pull requests
welcome; please keep both languages in step.
