# 调试与基准工具

## j8bcdbg：看字节码

`samples/j8bcdbg`：编译 JS、按函数列出字节码，格式照 V8 的 `--print-bytecode`（例子见[字节码](bytecode.md#看一眼真实的字节码)）。

```sh
cd samples/j8bcdbg
cargo run -- compile a.js                 # `-` 读标准输入
cargo run -- compile -e 'let s = 0; for (let i = 0; i < 10; i++) s += i; s'
cargo run -- compile a.js -o a.txt        # 写到文件
```

## JIT 的调试开关

| 环境变量 | 作用 |
|---|---|
| `J8_JIT_LOG=1` | 打印每次编译（指令数、字节数、耗时）与每次去优化（原因、位置） |
| `J8_JIT_IR=1` | 打印编译出的图 |
| `J8_JIT_BIN=<目录>` | 把每份机器码写成 `f<函数号>-<入口>.bin`，可以用 objdump 反汇编 |
| `J8_NOJIT=1` | 基准样例里只用解释器 |

## 三组基准

同一组 JS 基准（`samples/benchv8/bench/*.js`，11 个）在不同引擎上比，每个脚本的完成值是校验和，所有引擎必须一致：

| 样例 | 对比对象 | 说明 |
|---|---|---|
| `samples/benchv8` | V8（带 JIT）与 V8 `--jitless` | 经 rusty_v8 152.2.0，会下载 V8 预编译库 |
| `samples/benchjsc` | JavaScriptCore（带 JIT）与 `JSC_useJIT=false` | 用系统的 WebKitGTK 库（C API） |
| `samples/benchquickjs` | QuickJS（quickjs-ng 0.16.2） | 经 rquickjs 从源码编；QuickJS 没有 JIT |

```sh
cd samples/benchv8
cargo run --release                     # 三种引擎各跑一遍，打印对比表
cargo run --release -- --engine j8      # 只跑一个
cargo run --release -- --runs 10        # 每个基准跑几次，取最短（缺省 5）
```

- 每个引擎在自己的子进程里跑（`--jitless` 是进程级的开关）；
- 计时只算编译 + 执行，每次用新的执行环境，取最短的一次；
- 都是自成一包的 workspace，普通的 `cargo build --workspace` 不会拉 V8 的预编译库。

结果与分析见[性能](performance.md)。
