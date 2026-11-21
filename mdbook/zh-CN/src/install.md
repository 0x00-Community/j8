# 安装 dune

dune（连同跑包的 dunx）每个平台一个预编译的可执行文件，挂在每次
[j8 的 GitHub Release](https://github.com/0x00-Community/j8/releases) 上，同时发到 npm。

## macOS 和 Linux

```sh
curl -fsSL https://j8.app238.com/install | bash
```

脚本按本机选包（x64 或 arm64；Linux 上分 glibc 和 musl；Rosetta 下装原生版），用 Release 里的 `SHA256SUMS` 校验，
把 `dune` 和 `dunx` 链接放进 `~/.dune/bin`，再把这个目录写进 shell 启动文件的 `PATH`（bash、zsh 或 fish）。

装指定版本：

```sh
curl -fsSL https://j8.app238.com/install | bash -s v0.4.0
```

## Windows

```powershell
powershell -c "irm https://j8.app238.com/install.ps1 | iex"
```

把 `dune.exe` 和 `dunx.exe` 装到 `%USERPROFILE%\.dune\bin` 并加进用户 `PATH`。装指定版本：

```powershell
& ([scriptblock]::Create((irm https://j8.app238.com/install.ps1))) -Version v0.4.0
```

## npm

```sh
npm install -g @jabberwocky238/dune
```

npm 只装符合本机的平台包（`@jabberwocky238/dune-linux-x64`、`@jabberwocky238/dune-darwin-arm64`、
`@jabberwocky238/dune-win32-x64` ……），postinstall 脚本把其中的二进制放到位，`dune` 启动时不经过 Node。

## Cargo

```sh
cargo install j8dune     # crates.io 上 dune 已被占用；然后 ln -s dune ~/.cargo/bin/dunx
```

## 升级

```sh
dune upgrade             # 最新版
dune upgrade --tag v0.4.0
```

`dune upgrade` 从 GitHub 下载本平台的 Release、校验后替换自己（`dunx` 是副本而不是链接时一起换）。

## 环境变量

| 变量 | |
|---|---|
| `DUNE_INSTALL` | 安装脚本的安装根目录（缺省 `~/.dune`，二进制在其 `bin/`） |
| `DUNE_RELEASES` | 安装脚本从哪下载 Release（缺省 `https://github.com/0x00-Community/j8/releases`） |

## 平台

| 平台 | Release 包 | JIT |
|---|---|---|
| Linux x64（glibc 2.17+ / musl） | `dune-linux-x64.tar.gz` / `dune-linux-x64-musl.tar.gz` | 有 |
| Linux arm64（glibc 2.17+ / musl） | `dune-linux-arm64.tar.gz` / `dune-linux-arm64-musl.tar.gz` | 无 |
| macOS x64 / arm64 | `dune-darwin-x64.tar.gz` / `dune-darwin-arm64.tar.gz` | 仅 x64 |
| Windows x64 / arm64 | `dune-win32-x64.tar.gz` / `dune-win32-arm64.tar.gz` | 无 |

JIT 只在 Unix 上出 x86-64 机器码；其他平台 `--jit` 打一行提示后用解释器跑。

## 卸载

删掉 `~/.dune`（或 `%USERPROFILE%\.dune`）和脚本写进 shell 启动文件的几行（`# dune` 标记），或者
`npm uninstall -g @jabberwocky238/dune`。
