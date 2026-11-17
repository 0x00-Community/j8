# workerd

[English](README.md) · **简体中文**

用 [j8](https://github.com/0x00-Community/j8) 加载 Cloudflare Worker（`export default { fetch }` 的 ES 模块）与 Durable Object
的最小宿主（样例宿主）。

```sh
cargo run -- examples/counter.js --bind COUNTER=Counter --request "GET /c/inc?name=a"
cargo run -- examples/counter.js --bind COUNTER=Counter --port 8787   # 起 HTTP 服务
cargo run -- examples/chat.js --bind ROOM=ChatRoom --port 8787 --storage .chat   # WebSocket 聊天室
```

- Fetch API 子集、WebSocket 与 Durable Object 是 `src/prelude.js` 里的 JavaScript
- 每个 Durable Object 有键值存储（Rust 的 `KvStore`）和 SQL 存储（`ctx.storage.sql`，每个对象一个 SQLite 文件，放在
  `--storage` 目录下，不给就在内存里；`src/sql.rs`）
- `--bind NAME=Class` 把一个 Durable Object 类交给 worker 的 `env`
- 自成一个包（不在 j8 的 workspace 里），依赖 crates.io 上发布的 `j8`

## WebSocket

接口照 workerd：`new WebSocketPair()`、`server.accept()`（或休眠式接口 `ctx.acceptWebSocket(ws, tags)`，事件交给对象的
`webSocketMessage` / `webSocketClose`，配 `ctx.getWebSockets(tag)`、`serializeAttachment`）、`send`、`close`、
`addEventListener`，以及 `new Response(null, { status: 101, webSocket: client })`。被接受的一端在响应发出之前 send 的
消息先排队，耦合时最先发出。

服务器（`src/ws.rs`）跑在 j8 的事件循环上。每条连接的网络侧照 workerd 的 `api::WebSocket::Native` 布局：一个状态（等答复或
升级 ⇒ 已接受）、几个关闭标志、一条出站队列由 pump 线程写出。每条连接一个线程读帧，读到的消息只交给 JS 正在等的那个
promise（`server.recv(id)`），所以脚本没要就不往下读；没有消息时事件循环睡着。

`examples/chat.js` 是聊天室：消息先写进对象的 SQLite 表，再发给房间里所有人，新来的人先收到存下的历史。`tests/chat.rs`
加压跑它：八个客户端连同一个端口，两轮并发发言，中间全场沉默五秒（服务器不能占 CPU，之后要被下一条消息叫醒），每个
客户端看到的次序一致，最后在同一份存储上重启，历史按原次序重放。

```sh
cargo test --test chat
```

## 许可

MIT，随 [j8](https://github.com/0x00-Community/j8) 一起。
