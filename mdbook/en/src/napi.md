# Node-API

`crates/j8napi` implements [Node-API](https://nodejs.org/api/n-api.html) on j8: the C interface native addons (including
modules written with [napi-rs](https://napi.rs)) link against. The `napi_*` functions are exported under their own names;
embedders link them in (statically, or exported from the executable for addons loaded at run time).

## Use

```rust,ignore
// inside an `Isolate::enter`, with the agent of the context the addon belongs to
let exports = j8napi::Env::open(&mut agent, napi_register_module_v1)?;
agent.set_named(agent.context.global.into(), "addon", exports)?;
```

`Env::open` makes the addon's environment (living as long as the context's global object), calls the register function
and gives back its exports.

## How it maps onto j8

- A `napi_value` is a cell on the environment's handle stack holding the value's raw word; handles are valid for one
  entry into the isolate, during which nothing is collected.
- An addon's functions are constructible `NativeClosure`s: the engine makes `this` from `new.target.prototype`, so JS
  classes can extend the addon's classes.
- `napi_wrap` and type tags live in the object's private members; a collected host object queues its finalizer, which
  runs on the JS thread at the next callback.
- `napi_ref`, pending exceptions and deferreds hold persistent handles.

## Differences from Node

- A `napi_ref` whose count drops to 0 is still strong (the heap has no weak references yet).
- Strings are stored as UTF-8: lone surrogates in UTF-16 input are replaced.
- A Buffer is a plain `Uint8Array` (there is no Node `Buffer` class).
- Async work and thread-safe functions need an isolate with an event loop (`EventLoopSet`); each async work runs on a
  thread of its own. `napi_unref_threadsafe_function` doesn't let the event loop end early; there is no libuv loop
  (`napi_get_uv_event_loop` fails).

Turn it on in the facade with feature `napi`: `j8::napi`.
