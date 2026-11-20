# benchquickjs

[English](README.md) · **简体中文**

与 [benchv8](../benchv8) 同一组 JavaScript 基准（`../benchv8/bench/*.js`）在 [j8](https://github.com/0x00-Community/j8) 与
[QuickJS](https://github.com/quickjs-ng/quickjs)（quickjs-ng，由 [rquickjs](https://crates.io/crates/rquickjs) 从源码编译）上跑，
打印对比。QuickJS 没有 JIT，比的是解释器对解释器。

```sh
cd samples/benchquickjs
cargo run --release                         # 两个引擎都跑，打印对比表
cargo run --release -- --engine quickjs     # 只跑一个：j8 / quickjs
cargo run --release -- --runs 10            # 每个基准跑几次，取最短（缺省 5）
```

- 每个引擎在自己的子进程里跑；计时只算编译 + 执行，每次一个新的上下文
- 每个脚本的完成值是校验和，各引擎必须一致（与 V8 / JavaScriptCore 也一致，见 benchv8 / benchjsc）
- 自成 workspace（根 workspace 已排除）；编 QuickJS 需要 C 编译器

## 许可

MIT，随 [j8](https://github.com/0x00-Community/j8) 一起。
