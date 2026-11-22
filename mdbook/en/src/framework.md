# Framework: Isolate, Agent and values

`crates/j8framework` is the core layer: the traits the other layers implement and the structs / enums they share. It
**depends on no other j8 layer**; implementations live in the layer crates (`j8platform` implements `Platform`, and so on).

## Core types

| Name | Role | V8 |
|---|---|---|
| `Platform` / `Pool` | the threads and clocks the host lends the engine | `v8::Platform` |
| `Isolate` | an independent JS world: one GC heap (paragc) and a profiler | `v8::Isolate` |
| `Scope` | one entry into an isolate; values in it carry the `'gc` brand and can't leave it | `HandleScope` |
| `Value<'gc>` | a JS value: V8's `Local`, it can't leave the closure | `Local<Value>` |
| `Global` | a persistent handle (paragc `Root`): survives `enter` calls, released on drop | `Global<T>` |
| `ContextRoot` | the execution environment kept across slices | — |
| `Context` / `Realm` | the execution context: the global object and built-in prototypes (`Intrinsics`, from j8builtins) | `Context` |
| `Agent` | the context of one run; all JS operation semantics hang off it, calls back into JS go through `Invoke` | — |
| `Engine` | the entry the execution engine (j8executor) implements; resumable (`start` / `resume`) | — |
| `Profiler` | call / back-edge counters | — |
| `EventLoop` / `EventLoopSet` | JS event loops and the loop threads they run on ([Event loops](event-loop.md)) | — |

## Entering an isolate

```rust,ignore
let platform = DefaultPlatform::default();
let mut isolate = Isolate::new(&platform, CreateParams::default());
let mut executor = Executor::default();
let cx = isolate.enter(|scope| scope.persist(scope.context(&Builtins)));   // Global<Context>
let result = isolate.run(&mut executor, &cx, &Source { text: "1 + 2 * 3", file: "a.js" })?;
isolate.enter(|scope| {
    let v = scope.local(&result);       // Global ⇒ Value<'gc>, valid only in this closure
});
```

- `Isolate::enter(|scope| …)` enters once; `'gc` is that entry's **brand**. Nothing is collected during the closure;
  after it returns, a collection runs if enough was allocated (usually young, a full one every few).
- `Value<'gc>` can't leave the closure, **checked at compile time**: Rust lifetimes stand in for V8's `HandleScope`.
- To keep a value across entries, turn it into a `Global` with `scope.persist(v)` / `agent.global(v)`.

## Values: one NaN-boxed word

A `Value` is one 64-bit word (`ValueBits`):

- **numbers** are their own `f64` bits (every NaN is stored as `CANONICAL_NAN`);
- **everything else** lives in the negative NaN space: the top 16 bits are `0xFFF0 | (tag + 1)`, the low 48 bits the
  payload, a bool or a GC address (user-space addresses fit in 48 bits).

The `ValueTag`s: `UNDEFINED`, `NULL`, `BOOL`, `NUMBER`, `UNINIT` (a binding in its temporal dead zone; only in registers
and environment slots, checked away before reads and writes, it never reaches JS), `LAZY` (a property value computed on
first read), `STRING`, `SYMBOL`, `BIGINT`, `OBJECT`, `ACCESSOR`, `CONTEXT`.

"Is it a number", "is it an object" are one compare each, without a full decode. Registers, the accumulator,
environment slots and property slots all hold the same word, so JIT code reads and writes it directly and a `ValueCell`
stores it as is.

```rust,ignore
let v = Value::Number(1.5);
match v.kind() {
    ValueKind::Number(n) => …,
    ValueKind::Object(o) => …,
    _ => …,
}
```

## Agent: operation semantics

An `Agent` is the context of one run: the current realm, the heap's mutator and the engine's `Invoke`. The spec's
abstract operations are its methods (`ops.rs`): conversions (ToNumber, ToString, ToPrimitive …), operators
(`BinaryOp`, `CompareOp`, `UnaryOp`), property access (along the prototype chain, through the
[internal methods](objects.md)). The executor, the built-ins and binding code share them, so semantics are written once.

```rust,ignore
let s = a.to_string(v)?;
let f = a.get_named(obj, "then")?;
let r = a.call(f, this, &[arg])?;
let err = a.throw(ErrorKind::Type, "x is not a function");   // an Error; `?` it out of a host function to throw
```

Built-ins and host functions all have the signature `fn(&mut Agent, this, &[Value]) -> Result<Value, Error>`.

A built-in that calls the same function many times (`map`, `filter`, `reduce`, `sort`'s comparator …) resolves it once
with `a.callback(f, this)` and calls the returned `Callback` in the loop. The engine fills it in through
`Invoke::callback`: with the JIT, a plain function of up to three parameters goes straight to its compiled code (through
the function's stub, so a later compile or deopt is picked up); anything else falls back to `Agent::call`.

```rust,ignore
let cb = a.callback(f, this_arg);
for (k, v) in items {
    let r = cb.call(a, &[v, k.into(), o.into()])?;
}
```

## Error

| Variant | Meaning |
|---|---|
| `Error::Exception(g)` | JS threw the value `g` |
| `Error::Syntax(msg)` | the source doesn't parse (early errors included) |
| `Error::Unsupported(msg)` | syntax j8 doesn't support yet, with `file:line:column` |
| `Error::Terminated` | stopped by `IsolateHandle::terminate` |

## Terminating execution

`IsolateHandle::terminate` stops JS from another thread (V8's `TerminateExecution`): the execution layers notice the
request at checkpoints (function entries, back edges; in JIT code, back edges) and throw `Error::Terminated`. It is not
a JS exception: `catch` / `finally` can't catch it, and it goes all the way out. Leaving the outermost entry resets it,
so the isolate stays usable.

## Other parts

| File | Contents |
|---|---|
| `promise.rs` | promises and microtasks: resolve / reject, `then` reactions, driving async coroutines, draining the queue |
| `module.rs` | module records, import / export entries, `ResolvedBinding` |
| `proxy.rs` | Proxy: internal-method entry points dispatched to traps, with invariant checks |
| `typed.rs`, `backing.rs` | TypedArray elements; where an ArrayBuffer's bytes live (its own, host-mapped, shared between isolates) |
| `text.rs` | strings (see the [object model](objects.md)) |
| `parallel.rs` | parallel built-ins: plain-data work on large inputs split into chunks on the platform's pool (fork–join) |
| `layout.rs` | byte offsets into heap objects that machine code reads and writes directly |
| `lazy.rs` | properties computed on first read (V8's `AccessorInfo` on function maps: `name` isn't made with the closure) |
| `private.rs` | private class members `#x` |
