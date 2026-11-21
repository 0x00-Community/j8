# Platform

A `Platform` is what the host lends the engine: a thread pool, clocks, where `console.log` goes, and module loading.
`crates/j8platform` is the standard-library implementation, `DefaultPlatform`.

## DefaultPlatform

- **Thread pool**: [moremath](https://crates.io/crates/moremath)'s `ThreadMulti`, started once and reused; tasks may
  borrow the caller's data (through `j8framework::Pool`). Background tasks and parallel built-ins run on it. The GC heap
  has collector threads of its own.
- **Clocks**: `Instant` for the monotonic clock, `SystemTime` for `Date`.
- **Modules**: relative paths resolved against the importing file's directory, read from disk (no `node_modules`).
- `console.log` goes to standard output.

```rust,ignore
let platform = j8platform::DefaultPlatform::default();            // one worker thread per CPU
let small = j8platform::DefaultPlatform::from(NonZeroUsize::MIN);  // one thread
let mut isolate = j8framework::Isolate::new(&platform, Default::default());
```

## The traits

```rust,ignore
pub trait Pool: Sync {
    fn threads(&self) -> usize;
    fn run(&self, jobs: Vec<Task<'_>>);          // returns when the batch is done; tasks borrow the caller's data
}

pub trait Platform: Pool {
    fn now(&self) -> Duration;                    // monotonic clock
    fn wall_clock(&self) -> Duration;             // for Date
    fn print(&self, line: &str);                  // console.log
    fn load_module(&self, referrer: &str, specifier: &str) -> Option<(String, String)>;  // (normalized name, source)
}
```

(A sketch; the full signatures are in `j8framework/src/platform.rs`.)

## Stacking platforms

No platform holds async work (that lives in each runtime's isolate), so platforms stack in any order and runtimes can
share one. dune's `NodePlatform` wraps `DefaultPlatform`: it answers the specifiers of `node:` built-in modules and
passes the rest to the inner one:

```rust,ignore
let platform = Dune::platform(NodePlatform::from(DefaultPlatform::default()));
```

Methods worth overriding: `print` (send `console.log` elsewhere), `now` / `wall_clock` (a fake clock for tests),
`load_module` (packages, URLs, a virtual file system). See [Modules](modules.md#a-custom-platform).
