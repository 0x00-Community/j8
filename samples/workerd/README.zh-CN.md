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

网络层（`src/ws.rs`）是整个进程一个 epoll 反应堆线程（mio）：非阻塞地 accept、读请求、解析帧、写出，从不跑 JS。请求和消息
经单消费者的流交给事件循环，JS 去 await（`net.accept(server)`、`net.recv(id)`）；答复、升级、帧作为命令回到反应堆。每条连接
照 workerd 的 `api::WebSocket::Native`：等答复或升级的请求，然后是带关闭标志与出站缓冲的 WebSocket。没有消息时事件循环睡着。

`examples/chat.js` 是聊天室：消息先写进对象的 SQLite 表，再发给房间里所有人，新来的人先收到存下的历史。`tests/chat.rs`
加压跑它：八个客户端连同一个端口，两轮并发发言，中间全场沉默五秒（服务器不能占 CPU，之后要被下一条消息叫醒），每个
客户端看到的次序一致，最后在同一份存储上重启，历史按原次序重放。

```sh
cargo test --test chat
```

## 一个进程跑多个 Worker

`--instances N` 在一个进程里起 N 份 Worker：各自一个运行时（isolate、上下文、Durable Object、`<storage>/<i>`），各自一个端口
（`--port 0` 时各取空闲端口，按序号打印），都在同一个事件循环集合上，`--threads` 个 loop 线程（缺省每 CPU 一个）。
`tests/workerds.rs` 照此跑一千个聊天室：每间两个客户端，所有房间同时发言两轮、中间全场沉默五秒（整个进程不能占
CPU），最后重启、每间重放历史：

```sh
cargo test --release --test workerds -- --nocapture
```

Durable Object 的 SQLite 文件在构造对象之前、在事件循环线程之外建好（建库要同步磁盘），一千个房间同时开库也不卡
loop 线程。

## 许可

MIT，随 [j8](https://github.com/0x00-Community/j8) 一起。
