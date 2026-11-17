# benchv8

[English](README.md) · **简体中文**

同一组 JavaScript 基准（`bench/*.js`）在 [j8](https://github.com/0x00-Community/j8)、V8 `--jitless` 与带 JIT 的 V8（经 rusty_v8）
上跑，打印对比。结果与分析见 [benchmark.md](benchmark.md)。

```sh
cd samples/benchv8
cargo run --release                     # 三个引擎都跑，打印对比表
cargo run --release -- --engine j8      # 只跑一个：j8 / v8 / v8-jitless
cargo run --release -- --runs 10        # 每个基准跑几次，取最短（缺省 5）
```

- 每个引擎在自己的子进程里跑（`--jitless` 是进程级开关）；计时只算编译 + 执行，每次一个新的执行环境
- 每个脚本的完成值是校验和，各引擎必须一致
- 自成 workspace（根 workspace 已排除）：要下载 V8 的预编译库

## 许可

MIT，随 [j8](https://github.com/0x00-Community/j8) 一起。
