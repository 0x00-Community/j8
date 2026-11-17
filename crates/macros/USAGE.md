# j8macros usage

## `enums!("x.json")`

Generates enums from a JSON file under the calling crate's root directory (`CARGO_MANIFEST_DIR`). The JSON is the
single source of truth: ⛔ do not hand-write these enums; to change instructions, change only the JSON.
The caller is rebuilt automatically whenever the JSON changes (the generated code `include_bytes!`s it); unknown fields
are errors.

```rust
j8macros::enums!("insn.json");
```

Types used by the generated code (`Reg`, `Label`, `BinaryOp`, ...) must be in scope at the call site.

### JSON format

```json
{
  "enums": [
    {
      "name": "Insn",
      "doc": "An instruction.",
      "derive": ["Clone", "Copy", "Debug", "PartialEq"],
      "display": true,
      "accessors": [{"name": "target_mut", "ty": "Label", "doc": "Jump target"}],
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

| Key | Level | Meaning |
|---|---|---|
| `name` / `doc` | enum | Enum name and docs (required) |
| `derive` | enum | Passed as-is into `#[derive(...)]`; leave out traits that are written by hand (e.g. a bitwise `PartialEq`) |
| `repr` | enum | Integer type of the discriminant, passed as-is into `#[repr(..)]` (e.g. `"u16"`); if omitted, the compiler chooses and may use a niche in a field for the encoding (reading the discriminant then costs a few extra instructions) |
| `display` | enum | Generate `Display`: `mnemonic operand, operand` |
| `asm` | enum | Generate assembly (the inverse of `Display`): with `display` on ⇒ `FromStr` (`Err = AsmError`; operands are taken through `Operands` in field order, and each type implements `Operand`); without it ⇒ unit variants only, implementing `Operand` by variant name. `AsmError` / `Operand` / `Operands` must be in scope at the call site; `Option` fields need a `prefix` |
| `accessors` | enum | Generate `pub fn name(&mut self) -> Option<&mut ty>`: the first field of type `ty` in each variant, `None` if there is none. This is how `target_mut` (`ty: Label`) is produced, so adding an instruction with a `Label` field automatically makes it count as a jump |
| `name` / `doc` | variant | Variant name and docs |
| `mnemonic` | variant | Mnemonic for `Display`; with `display` on, use either this or a `show: name` field |
| `suffix` | variant | Used with a `show: name` field: the mnemonic becomes `name.suffix` (another operand form of the same group of names, e.g. `add.smi r2, 1` alongside `add r2`) |
| `default` | variant | `#[default]` |
| `fields` | variant | Field list; all with `name` ⇒ struct variant, none with `name` ⇒ tuple variant; mixing is an error |
| `ty` | field | Rust type |
| `prefix` | field | Text written before the value in `Display` (`fn#`, `extends `) |
| `show` | field | `display` (default, `{}`) / `debug` (`{:?}`) / `name` (`x.name()` replaces the mnemonic) |

An `Option<_>` field that is `None` is omitted entirely from `Display`.

## Binding macros: `#[j8::class]` / `#[j8::object]` / `#[j8::function]`

Each of the three macros has its own file (`class.rs` / `object.rs` / `function.rs`); the shared code that reads
`#[js(…)]`, analyzes signatures and generates the conversion wrapper is in `method.rs`.
Use them through the `j8` package at the repository root (`j8/src/lib.rs` re-exports the macros together with
j8binding's traits); the generated code uses `::j8::` paths.

### `#[j8::class]`: export a Rust type as a JS class

Placed on an `impl` block: generates a conversion wrapper (`fn(agent, this, args) -> Result<Value>`) for each method
and implements `j8::Class`. The type itself needs no changes; instances are host objects (`ObjectKind::Host`) holding a
Rust value.

```rust
struct Counter { n: f64 }

#[j8::class]                      // or #[j8::class("name")]; defaults to the type name
impl Counter {
    #[js(constructor)]            // `new Counter(…)`; a plain call throws a TypeError
    fn new(start: Option<f64>) -> Self { Self { n: start.unwrap_or(0.0) } }

    fn inc(&mut self, by: f64) -> f64 { self.n += by; self.n }       // prototype method; name converted to camelCase

    #[js(getter)]
    fn value(&self) -> f64 { self.n }

    #[js(setter, name = "value")]
    fn set_value(&mut self, v: f64) { self.n = v }

    fn zero() -> f64 { 0.0 }      // no self: static method on the constructor
}

let handle = Counter::install(&mut agent, global);     // global `Counter`; returns the constructor and prototype
let obj = handle.instance(&agent, Counter { n: 1.0 });  // create an instance from Rust
```

#### Inheritance: `#[j8::class(extends = Base)]`

```rust
struct Dog { base: Animal, tricks: u32 }

impl AsRef<Animal> for Dog { fn as_ref(&self) -> &Animal { &self.base } }
impl AsMut<Animal> for Dog { fn as_mut(&mut self) -> &mut Animal { &mut self.base } }

#[j8::class(extends = Animal)]       // or #[j8::class("Hound", extends = Animal)]
impl Dog { … }
```

`Dog.prototype`'s prototype is `Animal.prototype` and `Dog`'s is `Animal` (installed on the same target first if it
isn't there yet), so `instanceof` works and `Animal`'s methods run on a `Dog` through `AsRef` / `AsMut`. Chains of any
length work.

### `#[j8::object]`: export an `impl` block containing only associated functions as a plain object

```rust
struct Fs;

#[j8::object("fs")]
impl Fs {
    fn exists_sync(path: &str) -> bool { … }        // fs.existsSync
    #[js(getter)]
    fn sep() -> String { "/".into() }               // fs.sep (accessor)
}

Fs::install(&mut agent, global);                    // global `fs`
```

### `#[j8::function]`: export a function as a JS function

```rust
#[j8::function]                    // or #[j8::function("name")]
fn greet(name: &str) -> String { format!("hi {name}") }

// The original function is kept; a module of the same name is generated next to it,
// and `greet::NATIVE` is the host function
greet::install(&mut agent, global);
```

### `#[j8::module("node:fs", "fs")]`: export an object of functions as a native module

Like `#[j8::object]`, but answers to one or more module specifiers (the first is canonical). In a runtime declared
with `j8::runtime!` it is served to `import` (a default export plus one named export per member) and kept in the
hidden registry `globalThis.__j8_native["node:fs"]`, so a prelude's `require` can hand it out.

```rust
struct Fs;

#[j8::module("node:fs", "fs")]
impl Fs {
    fn exists_sync(path: &str) -> bool { … }
}
```

```js
import fs, { existsSync } from "node:fs";
```

### `j8::runtime!`: declare a runtime

```rust
j8::runtime! {
    /// The node-style runtime
    pub Dune {
        globals: [Process],          // #[j8::class] / #[j8::object] types, installed on the global object
        modules: [Fs, Paths],        // #[j8::module] types: import / require
        natives: [KvStore],          // bound types the prelude uses: globalThis.__j8_native.KvStore
        prelude: PRELUDE,            // JavaScript run after installing
    }
}

// Extending a runtime without touching it: the parent's contents and prelude come first
j8::runtime! {
    pub MyNode: Dune {
        modules: [Sqlite],
    }
}

let platform = MyNode::platform(DefaultPlatform::default());   // serves the native modules to `import`
let mut rt = MyNode::runtime(&platform, Executor::default())?;  // context built, everything installed, preludes run
let result = rt.eval("require('fs').existsSync('/')", "main.js")?;
println!("{}", rt.text(&result));
```

Every section is optional; they come in this order, each followed by a comma. `j8::Runtime` also has
`eval_module`, `run_until_idle` (the event loop for `async fn` bindings), `set("process.argv", value)` (a data
property at a dotted path from the global object, converted with `IntoJs`), `enter` (work with the context directly)
and `isolate`. `samples/dune` and
`samples/workerd` are built this way.

### Attributes and parameters

| Attribute | Meaning |
|---|---|
| `#[js(constructor)]` | Constructor (returns `Self` or `Result<Self, Error>`); without one, the class cannot be `new`ed |
| `#[js(getter)]` / `#[js(setter)]` | Accessor; a getter and setter with the same name form a pair |
| `#[js(static)]` | Static method (the default for methods without `self`) |
| `#[js(name = "…")]` | The name in JS |
| `#[js(skip)]` | Not exported |

Besides plain values, these handle types:

| Type | As a parameter | As a return value |
|---|---|---|
| `j8::Persistent` | keeps any JS value across `enter` calls (store it in the host struct; `.get(a)` reads it) | `&Persistent` gives the value back |
| `j8::Function` | a callable `Persistent` (`TypeError` if not callable); `.call(a, this, args)` | `&Function` |
| `j8::Bytes` | the bytes of an `ArrayBuffer` / typed array / `DataView` | a new `Uint8Array` |
| `j8::Promise<T>` | a JS promise (taken like `await` takes it) to `.await` in Rust: `Result<T, Error>`, the rejection as `Error::Exception` | — |
| `j8::Json<T>` (feature `json`) | any `serde::Deserialize` type, read through `JSON.stringify` (`TypeError` if it doesn't fit) | any `serde::Serialize` type, made through `JSON.parse` |

`async fn` bindings (associated functions with owned parameters) return a promise at once; the future runs on the
runtime's event loop, `j8::Runtime::run_until_idle`, which settles the promise with the result converted by `IntoJs`
and runs the microtasks. The work belongs to the runtime that started it (runtimes may share a platform); in an isolate
made without an event loop (not by `j8::Runtime`) the call throws a `TypeError`.

```rust
#[j8::object("later")]
impl Later {
    async fn fetch_text(url: String) -> String { … }
}
```

A `Persistent` is a GC root: a host object that stores a value reaching back to the host object keeps both alive until
the host object is dropped from Rust.

Parameters are taken by type: `&mut Agent` receives the current agent; `This` is the raw `this`; `Rest` is all remaining
arguments; `&str` is converted with ToString and then borrowed; anything else is converted in order with `FromJs`
(`f64` / `i32` / `u32` / `usize` / `bool` / `String` / `JsString` / `JsObject` / `Value` / `Option<T>` / `Vec<T>`).
Return values are converted with `IntoJs`; `Result<T, Error>` throws on error. Write lifetimes in parameters as `'gc`.
`&self` / `&mut self` methods that re-enter the same object throw a TypeError (no panic).

## Heap structs inside j8: `#[j8framework::heap]`

For j8's own heap types (not for embedders). Fields marked `#[mutable]` change in place: a field `x: T` is kept in
the cell `j8framework::Mutable` picks for `T` — `Value` ⇒ `ValueCell`, `Gc` / `Option<Gc>` (and aliases such as
`JsObject`) ⇒ a paragc `Slot`, `bool` / integers ⇒ atomics, `f64` ⇒ an `AtomicU64` of its bits — and gets
`x(&self) -> T` plus `set_x(&self, mc, v)` (through the write barrier where it matters). `#[mutable(set = name)]` renames
the setter (to wrap it in one of your own); `#[derive(Collect)]` is added when missing; `#[j8framework::heap(new)]`
also generates `new` taking every field in order.

```rust
#[j8framework::heap]
pub struct Object<'gc> {
    #[mutable(set = write_proto)]
    proto: Option<JsObject<'gc>>,      // proto() / write_proto(mc, p)
    #[mutable]
    non_extensible: bool,              // non_extensible() / set_non_extensible(mc, b)
    data: GcCell<'gc, ObjectState<'gc>>,
}
```

Write a cell only once its object is on the heap (the barrier records the cell's address); `new` fills cells before
that, without the barrier.

