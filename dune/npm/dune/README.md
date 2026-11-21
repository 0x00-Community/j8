# @jabberwocky238/dune

[English](README.md) · [简体中文](README.zh-CN.md)

**dune** is a JavaScript runtime built on [j8](https://github.com/0x00-Community/j8), with a Node-compatible layer and
a Web-standard layer. **dunx** (the same as `dune x`) runs a package's executable, installing it from the npm registry
when needed, like `npx` / `bunx`.

```sh
npm install -g @jabberwocky238/dune
dune app.js
dune -p '[1, 2, 3].map(x => x * 2)'
dunx cowsay hello
```

This package carries no binary itself: npm installs the one for your machine from `@jabberwocky238/dune-<platform>`
(`@jabberwocky238/dune-linux-x64`, `@jabberwocky238/dune-darwin-arm64`, `@jabberwocky238/dune-win32-x64`, ...), and the postinstall script puts it in place of
the Node placeholders, so `dune` and `dunx` start without Node. `dunx` is a link to `dune`. With `--ignore-scripts`
the placeholders start the binary through Node.

Without npm: `curl -fsSL https://j8.app238.com/install | bash` (Windows: `powershell -c "irm https://j8.app238.com/install.ps1 | iex"`), or `cargo install j8dune` (then `ln -s dune ~/.cargo/bin/dunx`). See [Installing dune](https://j8.app238.com/install.html).

MIT.
