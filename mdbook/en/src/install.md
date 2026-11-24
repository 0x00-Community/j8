# Installing dune

dune (and its package runner dunx) ships as one prebuilt executable per platform, attached to every
[j8 release on GitHub](https://github.com/0x00-Community/j8/releases) and published to npm.

## macOS and Linux

```sh
curl -fsSL https://j8.app238.com/install | bash
```

The script picks the build for your machine (x64 or arm64; the native build under Rosetta),
checks it against the release's `SHA256SUMS`, puts `dune` and the `dunx` link in `~/.dune/bin` and adds that directory
to `PATH` in your shell's startup file (bash, zsh or fish).

A given release:

```sh
curl -fsSL https://j8.app238.com/install | bash -s v0.4.0
```

## Windows

```powershell
powershell -c "irm https://j8.app238.com/install.ps1 | iex"
```

Installs `dune.exe` and `dunx.exe` to `%USERPROFILE%\.dune\bin` and adds it to the user `PATH`. A given release:

```powershell
& ([scriptblock]::Create((irm https://j8.app238.com/install.ps1))) -Version v0.4.0
```

## npm

```sh
npm install -g @jabberwocky238/dune
```

npm installs only the platform package that fits (`@jabberwocky238/dune-linux-x64`, `@jabberwocky238/dune-darwin-arm64`,
`@jabberwocky238/dune-win32-x64` …) and the postinstall script puts its binary in place, so `dune` starts without Node.

## Cargo

```sh
cargo install j8dune     # the name dune is taken on crates.io; then ln -s dune ~/.cargo/bin/dunx
```

## Upgrading

```sh
dune upgrade             # the latest release
dune upgrade --tag v0.4.0
```

`dune upgrade` downloads the release for its own platform from GitHub, verifies the checksum and replaces itself
(and `dunx` when it is a copy rather than a link).

## Settings

| Variable | |
|---|---|
| `DUNE_INSTALL` | install root for the scripts (default `~/.dune`; binaries in its `bin/`) |
| `DUNE_RELEASES` | where the scripts download releases from (default `https://github.com/0x00-Community/j8/releases`) |

## Platforms

| Platform | Release archive | JIT |
|---|---|---|
| Linux x64 (glibc 2.28+) | `dune-linux-x64.tar.gz` | yes |
| Linux arm64 (glibc 2.28+) | `dune-linux-arm64.tar.gz` | no |
| macOS x64 / arm64 | `dune-darwin-x64.tar.gz` / `dune-darwin-arm64.tar.gz` | x64 only |
| Windows x64 / arm64 | `dune-win32-x64.tar.gz` / `dune-win32-arm64.tar.gz` | no |

The JIT emits x86-64 machine code on Unix; elsewhere `--jit` prints a notice and runs on the interpreter.

## Uninstalling

Delete `~/.dune` (or `%USERPROFILE%\.dune`) and the lines the script added to your shell's startup file (marked
`# dune`), or `npm uninstall -g @jabberwocky238/dune`.
