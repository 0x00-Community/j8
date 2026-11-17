# j8platform

**English** · [简体中文](README.zh-CN.md)

The standard-library implementation of `j8framework::Platform` for [j8](https://github.com/0x00-Community/j8):
`DefaultPlatform`.

- Thread pool: a [moremath](https://crates.io/crates/moremath) `ThreadMulti`, started once and reused; tasks may borrow
  the caller's data. Background tasks and parallel built-ins run on it (the GC heap has its own collector threads)
- Clocks: `Instant` for the monotonic clock, `SystemTime` for `Date`
- Modules: relative specifiers resolve against the referrer's directory and are read from disk
- `console.log` prints to standard output

## Usage

```rust
let platform = j8platform::DefaultPlatform::default();          // one worker per CPU
let small = j8platform::DefaultPlatform::from(NonZeroUsize::MIN); // a single thread
let mut isolate = j8framework::Isolate::new(&platform, Default::default());
```

Embedders with their own event loop, clock or module loader implement `Platform` themselves.

## License

MIT, as part of [j8](https://github.com/0x00-Community/j8).
