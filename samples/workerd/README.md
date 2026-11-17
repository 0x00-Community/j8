# workerd

**English** · [简体中文](README.zh-CN.md)

A minimal host that loads Cloudflare Workers (ES modules with `export default { fetch }`) and Durable Objects with
[j8](https://github.com/0x00-Community/j8) (a sample host).

```sh
cargo run -- examples/counter.js --bind COUNTER=Counter --request "GET /c/inc?name=a"
cargo run -- examples/counter.js --bind COUNTER=Counter --port 8787   # an HTTP server
```

- The Fetch API subset and Durable Objects are JavaScript in `src/prelude.js`
- Each Durable Object's storage is the Rust `KvStore`, bound as `__j8_native.KvStore` with `#[j8::class]`
- `--bind NAME=Class` exposes a Durable Object class to the worker's `env`
- A package of its own (not part of the j8 workspace), built against the published `j8` crate

## License

MIT, as part of [j8](https://github.com/0x00-Community/j8).
