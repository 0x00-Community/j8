# Binding macros

The macros are in `crates/macros` (j8macros), written `#[j8::class]` and so on through the facade crate; their runtime
side is `crates/j8binding`, V8's `FunctionTemplate` / `ObjectTemplate`. Each macro has its own file (`class.rs` /
`object.rs` / `function.rs`); reading `#[js(…)]`, analyzing signatures and generating the conversion wrapper is shared in
`method.rs`.
A worked example focused on iterators and async in both directions: [Custom content with the macros](custom-host.md).

## `#[j8::class]`: a Rust type as a JS class

On an `impl` block: generates a conversion wrapper (`fn(agent, this, args) -> Result<Value>`) for each method and
implements `j8::Class`. The type needs no changes; instances are host objects (`ObjectKind::Host`) holding a Rust value.

```rust,ignore
struct Counter { n: f64 }

#[j8::class]                      // or #[j8::class("name")]; defaults to the type name
impl Counter {
    #[js(constructor)]            // `new Counter(…)`; a plain call throws a TypeError
    fn new(start: Option<f64>) -> Self { Self { n: start.unwrap_or(0.0) } }

    fn inc(&mut self, by: f64) -> f64 { self.n += by; self.n }   // prototype method, camelCase

    #[js(getter)]
    fn value(&self) -> f64 { self.n }

    #[js(setter, name = "value")]
    fn set_value(&mut self, v: f64) { self.n = v }

    fn zero() -> f64 { 0.0 }      // no self: a static method on the constructor
}

let handle = Counter::install(&mut agent, global);      // global `Counter`; returns the constructor and prototype
let obj = handle.instance(&agent, Counter { n: 1.0 });   // an instance made from Rust
```

### Inheritance: `#[j8::class(extends = Base)]`

```rust,ignore
struct Dog { base: Animal, tricks: u32 }

impl AsRef<Animal> for Dog { fn as_ref(&self) -> &Animal { &self.base } }
impl AsMut<Animal> for Dog { fn as_mut(&mut self) -> &mut Animal { &mut self.base } }

#[j8::class(extends = Animal)]
impl Dog { … }
```

`Dog.prototype`'s prototype is `Animal.prototype` and `Dog`'s is `Animal` (installed on the same target first if it isn't
there yet), so `instanceof` works and `Animal`'s methods run on a `Dog` through `AsRef` / `AsMut`. Chains of any length
work.

## `#[j8::object]`: associated functions as a plain object

```rust,ignore
struct Fs;

#[j8::object("fs")]
impl Fs {
    fn exists_sync(path: &str) -> bool { … }        // fs.existsSync
    #[js(getter)]
    fn sep() -> String { "/".into() }               // fs.sep (an accessor)
}
```

## `#[j8::function]`: one function

```rust,ignore
#[j8::function]                    // or #[j8::function("name")]
fn greet(name: &str) -> String { format!("hi {name}") }

greet::install(&mut agent, global);   // the function stays; a module of the same name holds `greet::NATIVE`
```

## `#[j8::module]`: native modules

Like `#[j8::object]`, but it answers one or more module specifiers (the first is canonical). In a runtime declared with
`j8::runtime!` it is served to `import` (a default export plus a named export per member) and kept in the hidden
registry `__j8_native["node:fs"]` for a prelude's `require`.

```rust,ignore
#[j8::module("node:fs", "fs")]
impl Fs {
    fn exists_sync(path: &str) -> bool { … }
}
```

```js
import fs, { existsSync } from "node:fs";
```

## Attributes

| Attribute | Meaning |
|---|---|
| `#[js(constructor)]` | the constructor (returns `Self` or `Result<Self, Error>`); without one the class can't be `new`ed |
| `#[js(getter)]` / `#[js(setter)]` | accessors; a getter and a setter of the same name form a pair |
| `#[js(static)]` | a static method (the default for methods without `self`) |
| `#[js(name = "…")]` | the name in JS |
| `#[js(skip)]` | not exported |

## Parameters and return values

Parameters are taken **by type**:

| Parameter type | Receives |
|---|---|
| `&mut Agent<'_, '_>` | the current agent (not a JS argument; any position) |
| `This` | the raw `this` |
| `Rest` | all remaining arguments |
| `&str` | ToString, then borrowed |
| anything else | converted in order with `FromJs`: `f64` / `i32` / `u32` / `usize` / `bool` / `String` / `JsString` / `JsObject` / `Value` / `Option<T>` / `Vec<T>` |

Return values go through `IntoJs`; `Result<T, Error>` throws on `Err`. Write lifetimes in parameters as `'gc`.

Handle types:

| Type | As a parameter | As a return value |
|---|---|---|
| `j8::Persistent` | keeps any JS value across `enter` calls (store it in the host struct; `.get(a)` reads it) | `&Persistent` gives the value back |
| `j8::Function` | a callable `Persistent` (`TypeError` if not callable); `.call(a, this, args)` | `&Function` |
| `j8::Bytes` | the bytes of an `ArrayBuffer` / TypedArray / `DataView` | a new `Uint8Array` |
| `j8::Promise<T>` | a JS promise (taken like `await` takes it), `.await`ed in Rust | — |
| `j8::Json<T>` (feature `json`) | any `serde::Deserialize` type, read through `JSON.stringify` | any `serde::Serialize` type, made through `JSON.parse` |

## Throwing

Take `&mut Agent`, return `Result<T, Error>`, build the error with `a.throw(kind, message)`:

```rust,ignore
fn must(a: &mut Agent<'_, '_>, name: &str) -> Result<String, Error> {
    std::env::var(name).map_err(|_| a.throw(ErrorKind::Type, &format!("{name} is not set")))
}
```

When the error is only known later, or you want structured errors (Node's `err.code`), return a type of your own whose
`IntoJs` throws. dune's `host.rs` does this with `Io<T>`:

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

The prelude reads the code off the front of the message and sets `err.code`. Since `IntoJs` runs on the runtime thread,
this works for the results of `async fn`s too.

## What a bound type must be

- A `#[j8::class]` type is stored inside the JS object, so it must be `Send + Sync + 'static`. Use `Arc` / atomics for
  state shared with threads (see `Timer` in `dune/src/web/timer.rs`).
- A method that calls back into JS that reaches the same object again (`&mut self` while already borrowed) throws a
  `TypeError` instead of panicking.
- ⛔ keeping a `Value` / `JsObject` in a struct: it would outlive its `enter`. Keep a `j8::Persistent` (or
  `j8::Function`). Each is a GC root: a cycle through a host object is only freed when that object is dropped.
