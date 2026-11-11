# r8macros 用法

## `enums!("x.json")`

从调用方 crate 根目录（`CARGO_MANIFEST_DIR`）下的 JSON 生成枚举。JSON 是唯一来源：⛔ 手写这些枚举，改指令只改 JSON。
JSON 一改调用方自动重编（生成代码 `include_bytes!` 了它）；未知字段报错。

```rust
r8macros::enums!("insn.json");
```

生成代码里用到的类型（`Reg`、`Label`、`BinaryOp`……）要在调用处可见。

### JSON 格式

```json
{
  "enums": [
    {
      "name": "Insn",
      "doc": "一条指令。",
      "derive": ["Clone", "Copy", "Debug", "PartialEq"],
      "display": true,
      "accessors": [{"name": "target_mut", "ty": "Label", "doc": "跳转目标"}],
      "variants": [
        {"name": "Ldar", "mnemonic": "ldar", "doc": "acc = reg", "fields": [{"ty": "Reg"}]},
        {"name": "Jump", "mnemonic": "jump", "fields": [{"ty": "Label"}]},
        {"name": "Call", "mnemonic": "call", "fields": [{"name": "callee", "ty": "Reg"}, {"name": "argc", "ty": "u16"}]},
        {"name": "Binary", "fields": [{"ty": "BinaryOp", "show": "name"}, {"ty": "Reg"}]},
        {"name": "CreateClass", "mnemonic": "create.class", "fields": [
          {"name": "func", "ty": "u32", "prefix": "fn#"},
          {"name": "parent", "ty": "Option<Reg>", "prefix": "extends "}
        ]}
      ]
    }
  ]
}
```

| 键 | 位置 | 含义 |
|---|---|---|
| `name` / `doc` | 枚举 | 枚举名、文档（必填） |
| `derive` | 枚举 | 原样进 `#[derive(...)]`；要手写的 trait（如按位比的 `PartialEq`）就不列 |
| `display` | 枚举 | 生成 `Display`：`助记符 操作数, 操作数` |
| `accessors` | 枚举 | 生成 `pub fn name(&mut self) -> Option<&mut ty>`：每个变体里第一个类型为 `ty` 的字段，没有给 `None`。`target_mut`（`ty: Label`）就是这样来的，加带 `Label` 字段的指令自动算跳转 |
| `name` / `doc` | 变体 | 变体名、文档 |
| `mnemonic` | 变体 | `Display` 的助记符；`display` 开时与 `show: name` 字段二选一 |
| `default` | 变体 | `#[default]` |
| `fields` | 变体 | 字段表；全带 `name` ⇒ 结构体变体，全不带 ⇒ 元组变体，混用报错 |
| `ty` | 字段 | Rust 类型 |
| `prefix` | 字段 | `Display` 时值前面的字（`fn#`、`extends `） |
| `show` | 字段 | `display`（缺省，`{}`）/ `debug`（`{:?}`）/ `name`（`x.name()` 代替助记符） |

`Option<_>` 字段为 `None` 时在 `Display` 里整个不出现。

## `#[r8binding::class]`（r8binding 的类绑定）

写在 `impl` 块上：每个方法生成一个转换外壳（`fn(agent, this, args) -> Result<Value>`），并实现 `r8binding::Class`。
类型本身不用改；实例是宿主对象（`ObjectKind::Host`），里面放一份 Rust 值。

```rust
struct Counter { n: f64 }

#[r8binding::class]               // 或 #[r8binding::class("名字")]，缺省用类型名
impl Counter {
    #[js(constructor)]            // `new Counter(…)`；普通调用抛 TypeError
    fn new(start: Option<f64>) -> Self { Self { n: start.unwrap_or(0.0) } }

    fn inc(&mut self, by: f64) -> f64 { self.n += by; self.n }       // 原型方法，名字转 camelCase

    #[js(getter)]
    fn value(&self) -> f64 { self.n }

    #[js(setter, name = "value")]
    fn set_value(&mut self, v: f64) { self.n = v }

    fn zero() -> f64 { 0.0 }      // 没有 self：构造器上的静态方法

    fn describe<'gc>(&self, a: &mut Agent<'gc, '_>, label: String) -> Result<Value<'gc>, Error> { … }
}

let handle = Counter::install(&mut agent, global);     // 全局 `Counter`，给出构造器与原型
let obj = handle.instance(&agent, Counter { n: 1.0 });  // Rust 这边建实例
```

| 标注 | 含义 |
|---|---|
| `#[js(constructor)]` | 构造器（返回 `Self` 或 `Result<Self, Error>`）；没有就是不能 `new` 的类 |
| `#[js(getter)]` / `#[js(setter)]` | 原型上的访问器，同名的配成一对 |
| `#[js(static)]` | 静态方法（没有 `self` 的缺省就是） |
| `#[js(name = "…")]` | JS 里的名字 |
| `#[js(skip)]` | 不导出 |

形参按类型取：`&mut Agent` 传当前 agent；`This` 是原样的 `this`；`Rest` 是剩下的全部实参；
`&str` 先 ToString 再借出；别的按 `FromJs` 依次转（`f64` / `i32` / `u32` / `usize` / `bool` / `String` / `JsString` / `JsObject` / `Value` /
`Option<T>` / `Vec<T>`）。返回值按 `IntoJs` 转，`Result<T, Error>` 出错就抛。形参里的生命周期写 `'gc`。
`&self` / `&mut self` 方法重入同一个对象时抛 TypeError（不 panic）。只要静态方法的可以 `install_namespace` 成普通对象。
