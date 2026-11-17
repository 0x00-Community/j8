# j8platform

[English](README.md) · **简体中文**

[j8](https://github.com/0x00-Community/j8) 的 `j8framework::Platform` 标准库实现：`DefaultPlatform`。

- 线程池：[moremath](https://crates.io/crates/moremath) 的 `ThreadMulti`，起一次反复用，任务可借用调用方数据；后台任务与并行内建函数跑在上面
  （GC 堆有自己的回收线程）
- 时钟：单调时钟用 `Instant`，`Date` 用 `SystemTime`
- 模块：相对路径按引用方所在目录解析，从磁盘读
- `console.log` 打到标准输出

## 用法

```rust
let platform = j8platform::DefaultPlatform::default();          // 每个 CPU 一个工作线程
let small = j8platform::DefaultPlatform::from(NonZeroUsize::MIN); // 单线程
let mut isolate = j8framework::Isolate::new(&platform, Default::default());
```

有自己事件循环、时钟或模块加载器的宿主自己实现 `Platform`。

## 许可

MIT，随 [j8](https://github.com/0x00-Community/j8) 一起。
