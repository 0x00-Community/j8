# @dune/dune

[English](README.md) · **简体中文**

**dune** 是基于 [j8](https://github.com/0x00-Community/j8) 的 JavaScript 运行时，带 Node 兼容层和 Web 标准层。
**dunx**（等同 `dune x`）跑包里的可执行文件，没装就从 npm registry 装，对标 `npx` / `bunx`。

```sh
npm install -g @dune/dune
dune app.js
dune -p '[1, 2, 3].map(x => x * 2)'
dunx cowsay hello
```

本包自身不带二进制：npm 按本机平台从 `@dune/<平台>`（`@dune/linux-x64`、`@dune/darwin-arm64`、`@dune/win32-x64` ……）
装上对应的一个，postinstall 脚本把它放到 Node 占位脚本的位置，`dune` 和 `dunx` 启动时不经过 Node；`dunx` 是 `dune`
的链接。用了 `--ignore-scripts` 时，占位脚本经 Node 启动二进制。

不用 npm：`cargo install j8dune`（再 `ln -s dune ~/.cargo/bin/dunx`）。

MIT。
