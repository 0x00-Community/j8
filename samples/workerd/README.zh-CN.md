# workerd

[English](README.md) · **简体中文**

用 [j8](https://github.com/0x00-Community/j8) 加载 Cloudflare Worker（`export default { fetch }` 的 ES 模块）与 Durable Object
的最小宿主（样例宿主）。

```sh
cargo run -- examples/counter.js --bind COUNTER=Counter --request "GET /c/inc?name=a"
cargo run -- examples/counter.js --bind COUNTER=Counter --port 8787   # 起 HTTP 服务
```

- Fetch API 子集与 Durable Object 是 `src/prelude.js` 里的 JavaScript
- 每个 Durable Object 的存储是 Rust 的 `KvStore`，用 `#[j8::class]` 绑成 `__j8_native.KvStore`
- `--bind NAME=Class` 把一个 Durable Object 类交给 worker 的 `env`
- 自成一个包（不在 j8 的 workspace 里），依赖 crates.io 上发布的 `j8`

## 许可

MIT，随 [j8](https://github.com/0x00-Community/j8) 一起。
