# 前言

这本书完整讲解 [j8](https://github.com/0x00-Community/j8)：一个用 Rust 从头写的 JavaScript 引擎，照着 V8 的分层来——
JS 先编成与 V8 Ignition 逐字节一致的字节码，由执行器解释，热函数和热循环交给推测式 JIT 出 x86-64 机器码；堆是
[paragc](https://crates.io/crates/paragc)。

## 安装 dune

```sh
curl -fsSL https://j8.app238.com/install | bash                 # macOS、Linux
powershell -c "irm https://j8.app238.com/install.ps1 | iex"     # Windows
npm install -g @jabberwocky238/dune                              # 有 npm 的地方
```

详见[安装 dune](install.md)。

## 怎么读

| 你想…… | 从这里读 |
|---|---|
| 先跑起来看看 | [快速开始](quickstart.md) |
| 理解整体设计 | [总体架构](architecture.md)，然后按「引擎内部」顺序读 |
| 用 j8 搭自己的运行时 | [搭一个宿主](embedding.md) 起的「嵌入 j8」部分 |
| 用 dune 跑脚本、跑 npm 包 | [dune 与 dunx](dune.md) |
| 给 j8 贡献代码 | [代码约定](conventions.md)、[test262 与测试](testing.md) |

## 约定

- 路径都相对 j8 仓库根，如 `crates/j8executor/src/ic.rs`。
- 文中「V8」指 V8 引擎本身；j8 对齐的版本是 rusty_v8 152.2.0 里的 V8 15.2.124。
- 本书描述的是仓库 **main 分支**（2026 年 11 月）。crates.io 上的 j8 0.4.0 早于 V8 字节码与推测式 JIT，下一个版本才包含它们。
- 数字（test262 通过数、基准耗时）写的是本书编写时的状态，以仓库为准。

本书源码在仓库的 `mdbook/` 目录，发现错误欢迎直接提 PR。
