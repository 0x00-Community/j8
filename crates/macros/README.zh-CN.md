# j8macros

[English](README.md) · **简体中文**

[j8](https://github.com/0x00-Community/j8) 的过程宏。

| 宏 | 生成什么 |
| --- | --- |
| `enums!("x.json")` | 从调用方 crate 下的 JSON 生成枚举（定义、按字段类型取值）与结构体（字段、关联常量），开 `binary` 再生成二进制编码；不生成文本形式；JSON 是唯一来源（`j8bytecode` 的全部类型） |
| `#[heap]` | j8 自己的堆结构体：`#[mutable]` 字段按 `j8framework::Mutable` 选的单元存（`ValueCell`、`Slot`、原子量），生成 getter / setter |
| `#[derive(Collect)]` | 堆类型的 paragc `Trace`；`#[collect(require_static)]` 跳过一个字段（放在类型上表示整个类型没有 GC 边）；有边的类型不许实现 `Drop` |
| `#[class]` / `#[object]` / `#[function]` / `#[module]` | Rust ⇄ JS 绑定，经门面 crate 写成 `#[j8::class]` 等；`#[j8::module("node:fs")]` 导出给 `import` / `require` 的原生模块，门面 crate 里的 `j8::runtime!` 把全局对象、模块、序言组装成一个运行时 |

JSON 格式与全部绑定属性见 [USAGE.md](USAGE.md)。

## 许可

MIT，随 [j8](https://github.com/0x00-Community/j8) 一起。
