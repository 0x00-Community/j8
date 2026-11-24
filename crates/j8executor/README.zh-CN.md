# j8executor

[English](README.md) · **简体中文**

[j8](https://github.com/0x00-Community/j8) 的字节码执行器：执行 `j8bytecode` 的累加器式寄存器字节码（`Executor<'p>`：`eval`，可续跑的
`start` / `resume`），并驱动 JS 事件循环（`EventLoop` / `EventLoopSet`）。

`WasmStore` 运行 WebAssembly：宿主函数 / 表 / 内存 / 全局、`instantiate`（导入链接、全局、表、内存、元素段和数据段、
start 函数）与 `invoke`。解释器每个函数只解码一次字节码，64 位寄存器槽放在同一个栈上（被调函数的帧从调用者的参数寄存器
开始，参数和结果都不拷贝），用显式帧栈，出错返回 `WasmTrap`。

## 怎么跑

- 显式帧栈（不靠宿主递归）；全部帧共用一个寄存器文件；被捕获的绑定放在堆上的 `Env` 链里
- 异常：每帧一个动态处理器栈；抛出时弹本帧的处理器，没有就展开到调用者
- 内联缓存（`ic.rs`）：属性读、写、加属性，按 shape 地址识别
- 分片执行：`Executor::eval` 每片最多跑固定条数，停下时把状态挂到根上、让堆回收，再接着跑
- `Tier` 钩子：JIT（`j8jit`）接管热函数与热循环

一个 `Executor` 只服务一个 isolate：它持有加载的函数，堆上的闭包按 `FuncId` 指回来。

## 用法

```rust
let mut executor = j8executor::Executor::default();
let result = executor.eval(&mut isolate, &cx, &j8framework::Source { text: "1 + 2", file: "a.js" })?;
```

## 许可

MIT，随 [j8](https://github.com/0x00-Community/j8) 一起。
