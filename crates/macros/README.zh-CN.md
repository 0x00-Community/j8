# j8macros

[English](README.md) · **简体中文**

[j8](https://github.com/0x00-Community/j8) 的过程宏。

| 宏 | 生成什么 |
| --- | --- |
| `bytecodes!("v8.json")` | V8 的字节码表（按 V8 编号的字节码、操作数类型、隐式读写累加器）由 `tools/v8bytecodes.py` 从 V8 头文件生成的 JSON 生成；给 `j8bytecode` 用 |
| `#[heap]` | j8 自己的堆结构体：`#[mutable]` 字段按 `j8framework::Mutable` 选的单元存（`ValueCell`、`Slot`、原子量），生成 getter / setter |
| `#[derive(Collect)]` | 堆类型的 paragc `Trace`；`#[collect(require_static)]` 跳过一个字段（放在类型上表示整个类型没有 GC 边）；有边的类型不许实现 `Drop` |
| `#[class]` / `#[object]` / `#[function]` / `#[module]` | Rust ⇄ JS 绑定，经门面 crate 写成 `#[j8::class]` 等；`#[j8::module("node:fs")]` 导出给 `import` / `require` 的原生模块，门面 crate 里的 `j8::runtime!` 把全局对象、模块、序言组装成一个运行时 |

JSON 格式与全部绑定属性见 [USAGE.md](USAGE.md)。

## 许可

MIT，随 [j8](https://github.com/0x00-Community/j8) 一起。
