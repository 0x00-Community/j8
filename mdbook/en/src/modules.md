# Modules

## ES modules from files

`rt.eval_module(text, file)` runs `text` as an ES module. `DefaultPlatform` resolves `import` relative to the importing
file (canonical paths, no `node_modules`). For other rules (packages, URLs, a virtual file system), write your own
`Platform::load_module`.

On the executor's side, loading, linking and evaluation are synchronous; import bindings read the exporter's slots live;
`ResolveExport` handles indirect exports, `export *` and ambiguity; namespace objects, dynamic `import()` and
`import.meta` are supported. Top-level `await` is not yet.

## Native modules in Rust: `#[j8::module]`

```rust,ignore
#[j8::module("myhost:kv", "kv")]
impl Kv {
    fn get(key: &str) -> Option<String> { /* … */ }
}

j8::runtime! { pub MyHost { modules: [Kv], } }
```

`import kv, { get } from "myhost:kv"` works through `HostPlatform`. A prelude reaches the same object as
`__j8_native["myhost:kv"]`, for example to implement `require`.

## A custom platform

dune's `node:` modules are JavaScript objects built by the prelude, not Rust types. dune serves them to `import` with a
platform of its own, `NodePlatform` (`dune/src/node/mod.rs`): `load_module` returns a small generated ES module that
re-exports an object from `__j8_native`, and passes every other specifier to the inner platform.

```rust,ignore
impl<P: Platform> Platform for NodePlatform<P> {
    fn load_module(&self, referrer: &str, specifier: &str) -> Option<(String, String)> {
        match Builtin::try_from(specifier) {
            // "const m = globalThis.__j8_native.builtin(\"fs\"); export default m; export const readFileSync = m.readFileSync; …"
            Ok(b) => Some((format!("node:{}", b.spec().0), b.source())),
            Err(()) => self.inner.load_module(referrer, specifier),
        }
    }
    fn now(&self) -> Duration { self.inner.now() }
    fn wall_clock(&self) -> Duration { self.inner.wall_clock() }
    fn print(&self, line: &str) { self.inner.print(line); }
}

impl<P: Pool> Pool for NodePlatform<P> {               // Platform requires Pool: forward it too
    fn threads(&self) -> usize { self.inner.threads() }
    fn run(&self, jobs: Vec<Task<'_>>) { self.inner.run(jobs); }
}
```

The first element of the pair is the module's normalized name, and modules are cached by it: a second `import` that
resolves to the same name gets the same module. A specifier that resolves to `None` fails the import with
`Error::Syntax("… Cannot find module '…'")`.

## CommonJS

j8 itself doesn't know CommonJS. dune's `require`, `module`, and `node_modules` / `package.json` resolution (`main` /
`exports` / `imports`) are JavaScript in the prelude (`dune/src/node/loader.js`); bare specifiers on the ESM side are
resolved in `dune/src/node/esm.rs`. See [dune](dune.md).
