# 快速开始

## 作为库

```sh
cargo add j8                                              # 缺省 feature `runtime`：执行器 + 内建对象 + 标准平台
cargo add j8 --features jit                               # 再加 x86-64 JIT（x86-64 Unix 和 Windows）
cargo add j8 --no-default-features --features compiler    # 只要 JS ⇒ 字节码
```

最小的求值：

```rust,ignore
use j8::builtins::Builtins;
use j8::executor::Executor;
use j8::frame::{CreateParams, Isolate, Source};
use j8::platform::DefaultPlatform;

let platform = DefaultPlatform::default();
let mut isolate = Isolate::new(&platform, CreateParams::default());
let executor = Executor::default();
let cx = isolate.enter(|scope| scope.mc.stash::<ContextBrand>(scope.context(&Builtins)));
let result = executor.eval(&mut isolate, &cx, &Source { text: "1 + 2 * 3", file: "a.js" })?;
```

真正的宿主一般不直接摆弄 Isolate，而是用 `j8::runtime!` 声明一个运行时，见[搭一个宿主](embedding.md)。

### feature 一览

| feature | 带上什么 |
|---|---|
| `runtime`（缺省） | `executor` + `builtins` + `platform` |
| `bytecode` | `j8::bytecode`：字节码定义、编码 / 解码 |
| `compiler` | `j8::compiler`：JS ⇒ 字节码（含 `bytecode`） |
| `executor` | `j8::executor`：字节码执行器（含 `compiler`） |
| `builtins` / `platform` | 内建对象 / 标准库平台 |
| `jit` | `j8::jit`：JIT（x86-64 Unix 和 Windows，含 `executor`） |
| `json` | `j8::Json<T>`：经 serde 以 JSON 形状过界的 Rust 值 |
| `napi` | `j8::napi`：Node-API |
| `taskqueue` / `lockfree` | 任务队列 / 无锁基础件 |
| `full` | 以上全部 |

## 用 dune 跑 JS

```sh
curl -fsSL https://j8.app238.com/install | bash   # 或 npm i -g @jabberwocky238/dune，见「安装 dune」
dune app.js                    # Node 兼容层（缺省）
dune --web app.mjs             # 只要 Web 标准接口
dune -p '[1, 2, 3].map(x => x * 2)'
dune --jit bench.js            # 打开 JIT
dune                           # REPL
dunx cowsay hello              # 跑 npm 包里的可执行文件
```

详见 [dune 与 dunx](dune.md)。

## 从源码

```sh
git clone https://github.com/0x00-Community/j8 && cd j8
cargo build --release
git submodule update --init                       # 拉 test262
cargo test --release --test test262               # 跑回归基线
cd dune && cargo run --release -- examples/node.js
cd samples/j8bcdbg && cargo run -- compile -e 'let s = 0; for (let i = 0; i < 10; i++) s += i; s'
```

`dune/` 与 `samples/*` 各自成包、不在 workspace 里，像嵌入方一样按版本依赖 crates.io 上的 `j8`。
