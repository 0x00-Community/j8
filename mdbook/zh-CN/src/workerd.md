# workerd 与 jumpheap

## workerd

`samples/workerd`：用 j8 加载 Cloudflare Worker（`export default { fetch }` 的 ES 模块）与 Durable Object 的样例宿主。

```sh
cargo run -- examples/counter.js --bind COUNTER=Counter --request "GET /c/inc?name=a"
cargo run -- examples/counter.js --bind COUNTER=Counter --port 8787          # 起 HTTP 服务
cargo run -- examples/chat.js --bind ROOM=ChatRoom --port 8787 --storage .chat   # WebSocket 聊天室
```

- Fetch API 子集、WebSocket 与 Durable Object 是 `src/prelude.js` 里的 JavaScript；
- 每个 Durable Object 有键值存储（Rust 的 `KvStore`）和 SQL 存储（`ctx.storage.sql`，每个对象一个 SQLite 文件，放在
  `--storage` 目录下，不给就在内存里；`src/sql.rs`）；
- `--bind NAME=Class` 把一个 Durable Object 类交给 worker 的 `env`。

### WebSocket

接口照 workerd：`new WebSocketPair()`、`server.accept()`（或休眠式的 `ctx.acceptWebSocket(ws, tags)`，事件交给对象的
`webSocketMessage` / `webSocketClose`，配 `ctx.getWebSockets(tag)`、`serializeAttachment`）、`send`、`close`、
`addEventListener`，以及 `new Response(null, { status: 101, webSocket: client })`。

网络层（`src/ws.rs`）是整个进程一个 epoll 反应堆线程（mio）：非阻塞地 accept、读请求、解析帧、写出，从不跑 JS。请求与消息
经单消费者的流交给事件循环，JS 去 await（`net.accept(server)`、`net.recv(id)`）；答复、升级、帧作为命令回到反应堆。
没有消息时事件循环睡着。

`examples/chat.js` 是聊天室：消息先写进对象的 SQLite 表，再发给房间里所有人，新来的人先收到存下的历史。`tests/chat.rs`
加压跑它：八个客户端、两轮并发发言、中间全场沉默五秒（服务器不能占 CPU）、每个客户端看到的次序一致、最后在同一份存储上重启、
历史按原次序重放。

### 一个进程跑一千个 Worker

`--instances N` 在一个进程里起 N 份 Worker：各自一个运行时（isolate、上下文、Durable Object、`<storage>/<i>`），各自一个端口，
都在同一个[事件循环集合](event-loop.md)上，`--threads` 个 loop 线程。`tests/workerds.rs` 照此跑一千个聊天室：所有房间同时发言
两轮、中间全场沉默五秒（整个进程不能占 CPU）、最后重启、每间重放历史。

所有实例共用一份 `CodeCache`（序言与 Worker 的模块只编译一次）和一个 epoll 反应堆线程。

## jumpheap

`samples/jumpheap`：几个常驻的 j8 worker 分在不同进程里，**JS 对象互相引用对方的堆**。

每个进程有一个 paragc **公开堆**（`/dev/shm` 里的内存文件）；它的 worker（workerd 式的 `export default { fetch }`）在那里建
JS 矩阵，经 HTTP 发给别的 worker，也监听别人发来的。收到的是一个 `object` 事件，带一个 JS `Matrix`：对象在接收方自己的
isolate 堆里，各行是**指着发送方堆**的 `Float64Array`，不是拷贝。上面的所有计算都是普通 JS。

```sh
cargo run --release -- examples/matrix.js --name wa --dir /dev/shm/jumpheap   # 打印端口
```

| JS 接口 | |
|---|---|
| `await heaps.matrix(rows)` | 本进程堆里新建 `rows` × 100000 的全零 `Matrix` |
| `matrix.data[i]` | 第 `i` 行：公开堆内存（本进程的或别的进程的）上的 `Float64Array` |
| `matrix.rows`、`matrix.cols`、`matrix.local` | 形状；是否在本进程堆里 |
| `await heaps.send(url, matrix, name, task)` | 把 `matrix` 交给 `url` 处的 worker，得到它的回应 |
| `heaps.addEventListener('object', e => …)` | 收到矩阵：`e.object`、`e.name`、`e.task`；`e.respondWith(value)` 回给发送方 |

收到的矩阵在发送方堆里占一个入口，只要接收方还持有它任何一行的 buffer，各行就活着；发送方进程死后，它写进别的堆的内容还在。
`tests/three.rs` 起三个 worker 互相发矩阵、中途杀掉一个，检查结果仍然正确。
