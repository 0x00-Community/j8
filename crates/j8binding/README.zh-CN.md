# j8binding

[English](README.md) · **简体中文**

[j8](https://github.com/0x00-Community/j8) 的 Rust ⇄ JavaScript 绑定，对应 V8 的 `FunctionTemplate` / `ObjectTemplate`。
代码一般由宏 `#[j8::class]` / `#[j8::object]` / `#[j8::function]` 生成（宏在 `j8macros`，由门面 crate `j8` 导出）。

- `FromJs` / `IntoJs`：参数与返回值的转换（数字、布尔、字符串、`Option`、`Vec`、对象、`Result`……）
- `Class` / `Object`：把 Rust 类型导出为 JS 类 / 一组函数构成的普通对象
- 宿主对象：持有 Rust 值的 JS 对象（`Box<dyn Any + Send + Sync>`，外面是无锁的借用标志），方法里以 `&self` / `&mut self` 取出；
  重入的 `&mut self` 调用抛 `TypeError`

## 用法

```rust
struct Counter { n: f64 }

#[j8::class]
impl Counter {
    #[js(constructor)]
    fn new(start: Option<f64>) -> Self { Self { n: start.unwrap_or(0.0) } }
    fn inc(&mut self, by: f64) -> f64 { self.n += by; self.n }
    #[js(getter)]
    fn value(&self) -> f64 { self.n }
}

let handle = Counter::install(&mut agent, global);   // 全局 `Counter`
```

宿主类型必须是 `Send + Sync`（堆的规矩）。全部属性见 `crates/macros/USAGE.md`。

## 许可

MIT，随 [j8](https://github.com/0x00-Community/j8) 一起。
