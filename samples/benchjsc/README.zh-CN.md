# benchjsc

[English](README.md) · **简体中文**

与 [benchv8](../benchv8) 同一组 JavaScript 基准（`../benchv8/bench/*.js`）在 [j8](https://github.com/0x00-Community/j8)、
关掉 JIT 的 JavaScriptCore（`JSC_useJIT=false`，只剩 LLInt）与带 JIT 的 JavaScriptCore 上跑，打印对比。

```sh
cd samples/benchjsc
cargo run --release                     # 三个引擎都跑，打印对比表
cargo run --release -- --engine j8      # 只跑一个：j8 / jsc / jsc-nojit
cargo run --release -- --runs 10        # 每个基准跑几次，取最短（缺省 5）
```

- JavaScriptCore 用 WebKitGTK 的系统库、走 C API：装运行库即可（Debian / Ubuntu 上是 `libjavascriptcoregtk-6.0-1`），
  不需要 -dev 包，按文件名链接
- j8 带 JIT 跑（`J8_NOJIT=1` 只用解释器）
- 每个引擎在自己的子进程里跑（JSC 每个进程只读一次 `JSC_*` 选项）；计时只算编译 + 执行，每次一个新的全局上下文
- 每个脚本的完成值是校验和，各引擎必须一致（与 V8 也一致，见 benchv8）
- 自成 workspace（根 workspace 已排除）

## 许可

MIT，随 [j8](https://github.com/0x00-Community/j8) 一起。
