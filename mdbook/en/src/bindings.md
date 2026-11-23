# Binding macros

The macros are in `crates/macros` (j8macros), written `#[j8::class]` and so on through the facade crate; their runtime
side is `crates/j8binding`, V8's `FunctionTemplate` / `ObjectTemplate`. Each macro has its own file (`class.rs` /
`object.rs` / `function.rs`); reading `#[js(…)]`, analyzing signatures and generating the conversion wrapper is shared in
`method.rs`.
A worked example focused on iterators and async in both directions: [Custom content with the macros](custom-host.md).

## `#[j8::class]`: a Rust type as a JS class

On an `impl` block: generates a conversion wrapper (`fn(scope, this, args) -> Result<Value>`) for each method and
implements `j8::Class`. Instances are host objects (`ObjectKind::Host`): the Rust value lives inside the JS object, in the
GC heap, so the type derives `j8::Trace` and the collector sees every JS value it holds.

```rust,ignore
#[derive(j8::Trace)]
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

let handle = Counter::install(&mut scope, global);      // global `Counter`; returns the constructor and prototype
let obj = handle.instance(&scope, Counter { n: 1.0 });   // an instance made from Rust
```

### Inheritance: `#[j8::class(extends = Base)]`

```rust,ignore
#[derive(j8::Trace)]
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

greet::install(&mut scope, global);   // the function stays; a module of the same name holds `greet::NATIVE`
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
| `&mut Scope<'_, '_>` | the current scope (not a JS argument; any position) |
| `This` | the raw `this` |
| `Rest` | all remaining arguments |
| `&str` | ToString, then borrowed |
| anything else | converted in order with `FromJs`: `f64` / `i32` / `u32` / `usize` / `bool` / `String` / `JsString` / `JsObject` / `Value` / `Option<T>` / `Vec<T>` |

Return values go through `IntoJs`; `Result<T, Error>` throws on `Err`. Write lifetimes in parameters as `'scope`.

Handle types:

| Type | As a parameter | As a return value |
|---|---|---|
| `Value<'scope>` / `JsObject<'scope>` | any JS value; as a field of a class it is a traced edge | the value itself |
| `j8::Function<'scope>` | a callable value (`TypeError` if not callable); `.call(a, this, args)`; as a field, a traced edge | the function |
| `ScopeGlobal<'p, ValueBrand>` | a root: keeps a JS value for code outside the heap (a thread, a channel, a Rust struct); `a.local(&g)` reads it | `&ScopeGlobal` gives the value back |
| `j8::Bytes` | the bytes of an `ArrayBuffer` / TypedArray / `DataView` | a new `Uint8Array` |
| `j8::Promise<T>` | a JS promise (taken like `await` takes it), `.await`ed in Rust | — |
| `j8::Json<T>` (feature `json`) | any `serde::Deserialize` type, read through `JSON.stringify` | any `serde::Serialize` type, made through `JSON.parse` |

## Throwing

Take `&mut Scope`, return `Result<T, Error>`, build the error with `a.throw(kind, message)`:

```rust,ignore
fn must(a: &mut Scope<'_, '_>, name: &str) -> Result<String, Error> {
    std::env::var(name).map_err(|_| a.throw(ErrorKind::Type, &format!("{name} is not set")))
}
```

When the error is only known later, or you want structured errors (Node's `err.code`), return a type of your own whose
`IntoJs` throws. dune's `host.rs` does this with `Io<T>`:

```rust,ignore
pub struct Io<T>(pub Result<T, Failure>);

impl<'scope, T: IntoJs<'scope>> IntoJs<'scope> for Io<T> {
    fn into_js(self, a: &mut Scope<'scope, '_>) -> Result<Value<'scope>, Error> {
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

- A `#[j8::class]` type is stored inside the JS object, in the GC heap: it is `Send + Sync`, derives `j8::Trace`, and
  may carry the `'scope` lifetime (`struct Hooks<'scope>`, `impl<'scope> Hooks<'scope>`). Its JS values are fields
  (`Value<'scope>`, `j8::Function<'scope>`, …) that the collector traces and moves with the object. Fields that hold
  no JS value and are not `Trace` (an `Arc`, a connection) are marked `#[collect(require_static)]`. Use `Arc` / atomics
  for state shared with threads (see `Timer` in `dune/src/web/timer.rs`).
- A method that calls back into JS that reaches the same object again (`&mut self` while already borrowed) throws a
  `TypeError` instead of panicking.
- A class's JS fields are edges, not roots, so a cycle through a host object (a callback that captures the object
  holding it) is collected like any other garbage. A `ScopeGlobal` is a root: keep it only outside the heap, never in a
  class field.
- The host object and its JS wrapper are one object: while it lives, every path to it gives the same JS object back.
- `Drop` of a class must not read its JS fields: when it runs they may already be gone.
- Lifetimes: `'scope` is the one lifetime of heap values; a class that holds none needs no lifetime at all.

```rust,ignore
#[derive(Default, j8::Trace)]
struct Hooks<'scope> {
    on: Option<j8::Function<'scope>>,
    kept: Option<Value<'scope>>,
}

#[j8::class]
impl<'scope> Hooks<'scope> {
    #[js(constructor)]
    fn new() -> Self { Self::default() }

    fn set(&mut self, f: j8::Function<'scope>, keep: Value<'scope>) { self.on = Some(f); self.kept = Some(keep); }

    fn fire(&self, a: &mut Scope<'scope, '_>, x: f64) -> Result<Value<'scope>, Error> {
        match self.on {
            Some(f) => f.call(a, Value::Undefined, &[x.into(), self.kept.unwrap_or(Value::Undefined)]),
            None => Ok(Value::Undefined),
        }
    }
}
```
