# 绑定宏

宏在 `crates/macros`（j8macros），经门面 crate 写成 `#[j8::class]` 等；运行时支撑在 `crates/j8binding`，对应 V8 的
`FunctionTemplate` / `ObjectTemplate`。三个宏各一个文件（`class.rs` / `object.rs` / `function.rs`），读 `#[js(…)]`、
分析签名、生成转换包装的共用代码在 `method.rs`。

## `#[j8::class]`：Rust 类型导出为 JS 类

放在 `impl` 块上：为每个方法生成转换包装（`fn(agent, this, args) -> Result<Value>`）并实现 `j8::Class`。类型本身不用改；
实例是持有 Rust 值的宿主对象（`ObjectKind::Host`）。

```rust,ignore
struct Counter { n: f64 }

#[j8::class]                      // 或 #[j8::class("name")]；缺省是类型名
impl Counter {
    #[js(constructor)]            // `new Counter(…)`；直接调用抛 TypeError
    fn new(start: Option<f64>) -> Self { Self { n: start.unwrap_or(0.0) } }

    fn inc(&mut self, by: f64) -> f64 { self.n += by; self.n }   // 原型方法，名字转 camelCase

    #[js(getter)]
    fn value(&self) -> f64 { self.n }

    #[js(setter, name = "value")]
    fn set_value(&mut self, v: f64) { self.n = v }

    fn zero() -> f64 { 0.0 }      // 没有 self：构造器上的静态方法
}

let handle = Counter::install(&mut agent, global);      // 全局 `Counter`，返回构造器与原型
let obj = handle.instance(&agent, Counter { n: 1.0 });   // 从 Rust 建实例
```

### 继承：`#[j8::class(extends = Base)]`

```rust,ignore
struct Dog { base: Animal, tricks: u32 }

impl AsRef<Animal> for Dog { fn as_ref(&self) -> &Animal { &self.base } }
impl AsMut<Animal> for Dog { fn as_mut(&mut self) -> &mut Animal { &mut self.base } }

#[j8::class(extends = Animal)]
impl Dog { … }
```

`Dog.prototype` 的原型是 `Animal.prototype`，`Dog` 的原型是 `Animal`（还没装的话先装到同一目标上），所以 `instanceof`
成立，`Animal` 的方法经 `AsRef` / `AsMut` 在 `Dog` 上跑。链多长都行。

## `#[j8::object]`：只有关联函数的 `impl` 导出为普通对象

```rust,ignore
struct Fs;

#[j8::object("fs")]
impl Fs {
    fn exists_sync(path: &str) -> bool { … }        // fs.existsSync
    #[js(getter)]
    fn sep() -> String { "/".into() }               // fs.sep（访问器）
}
```

## `#[j8::function]`：导出一个函数

```rust,ignore
#[j8::function]                    // 或 #[j8::function("name")]
fn greet(name: &str) -> String { format!("hi {name}") }

greet::install(&mut agent, global);   // 原函数保留，旁边生成同名模块，`greet::NATIVE` 是宿主函数
```

## `#[j8::module]`：原生模块

和 `#[j8::object]` 一样，但应答一个或多个模块说明符（第一个是规范名）。在 `j8::runtime!` 声明的运行时里，它供给
`import`（一个缺省导出加每个成员一个具名导出），并放进隐藏注册表 `__j8_native["node:fs"]`，供序言的 `require` 使用。

```rust,ignore
#[j8::module("node:fs", "fs")]
impl Fs {
    fn exists_sync(path: &str) -> bool { … }
}
```

```js
import fs, { existsSync } from "node:fs";
```

## 属性

| 属性 | 含义 |
|---|---|
| `#[js(constructor)]` | 构造器（返回 `Self` 或 `Result<Self, Error>`）；没有它，这个类不能 `new` |
| `#[js(getter)]` / `#[js(setter)]` | 访问器；同名的 getter 与 setter 组成一对 |
| `#[js(static)]` | 静态方法（没有 `self` 的方法缺省就是） |
| `#[js(name = "…")]` | JS 里的名字 |
| `#[js(skip)]` | 不导出 |

## 参数与返回值

参数按**类型**取：

| 参数类型 | 拿到什么 |
|---|---|
| `&mut Agent<'_, '_>` | 当前 agent（不算 JS 实参，放在哪个位置都行） |
| `This` | 原始的 `this` |
| `Rest` | 剩下的全部实参 |
| `&str` | ToString 后借用 |
| 其他 | 依次用 `FromJs` 转换：`f64` / `i32` / `u32` / `usize` / `bool` / `String` / `JsString` / `JsObject` / `Value` / `Option<T>` / `Vec<T>` |

返回值用 `IntoJs` 转换；`Result<T, Error>` 在 `Err` 时抛出。参数里的生命周期写成 `'gc`。

句柄类型：

| 类型 | 作参数 | 作返回值 |
|---|---|---|
| `j8::Persistent` | 跨 `enter` 保留任意 JS 值（存进宿主结构体；`.get(a)` 读） | `&Persistent` 把值还回去 |
| `j8::Function` | 可调用的 `Persistent`（不可调用抛 `TypeError`）；`.call(a, this, args)` | `&Function` |
| `j8::Bytes` | `ArrayBuffer` / TypedArray / `DataView` 的字节 | 新 `Uint8Array` |
| `j8::Promise<T>` | JS promise（像 `await` 那样接收），在 Rust 里 `.await` | — |
| `j8::Json<T>`（feature `json`） | 任何 `serde::Deserialize` 类型，经 `JSON.stringify` 读 | 任何 `serde::Serialize` 类型，经 `JSON.parse` 造 |

## 抛异常

拿 `&mut Agent`、返回 `Result<T, Error>`，用 `a.throw(kind, message)` 造错误：

```rust,ignore
fn must(a: &mut Agent<'_, '_>, name: &str) -> Result<String, Error> {
    std::env::var(name).map_err(|_| a.throw(ErrorKind::Type, &format!("{name} is not set")))
}
```

错误要晚些才知道、或者想要结构化的错误（Node 的 `err.code`）时，返回一个自己的类型，让它的 `IntoJs` 去抛。dune 的
`host.rs` 用 `Io<T>` 这样做：

```rust,ignore
pub struct Io<T>(pub Result<T, Failure>);

impl<'gc, T: IntoJs<'gc>> IntoJs<'gc> for Io<T> {
    fn into_js(self, a: &mut Agent<'gc, '_>) -> Result<Value<'gc>, Error> {
        match self.0 {
            Ok(v) => v.into_js(a),
            Err(f) => Err(a.throw(f.kind, &f.message)),   // "ENOENT: no such file or directory, open 'a.txt'"
        }
    }
}
```

序言再从消息开头读出 code、设到 `err.code` 上。因为 `IntoJs` 在运行时线程上跑，这对 `async fn` 的结果也适用。

## 绑定类型的要求

- `#[j8::class]` 类型存在 JS 对象里，所以必须 `Send + Sync + 'static`。和线程共享的状态用 `Arc` / 原子量
  （见 `dune/src/web/timer.rs` 的 `Timer`）。
- 方法回调 JS、JS 又碰到同一个对象（已借用时再要 `&mut self`）会抛 `TypeError`，不会 panic。
- ⛔ 在结构体里存 `Value` / `JsObject`：它会活过它的 `enter`。存 `j8::Persistent`（或 `j8::Function`）。它们都是 GC 根：
  经由宿主对象形成的环，要等那个宿主对象被丢掉才释放。
