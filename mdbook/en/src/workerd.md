# workerd and jumpheap

## workerd

`samples/workerd`: a sample host that loads Cloudflare Workers (ES modules with `export default { fetch }`) and Durable
Objects on j8.

```sh
cargo run -- examples/counter.js --bind COUNTER=Counter --request "GET /c/inc?name=a"
cargo run -- examples/counter.js --bind COUNTER=Counter --port 8787             # an HTTP server
cargo run -- examples/chat.js --bind ROOM=ChatRoom --port 8787 --storage .chat  # a WebSocket chat room
```

- the Fetch API subset, WebSocket and Durable Objects are JavaScript in `src/prelude.js`;
- every Durable Object has key-value storage (Rust's `KvStore`) and SQL storage (`ctx.storage.sql`, one SQLite file per
  object under `--storage`, or in memory without it; `src/sql.rs`);
- `--bind NAME=Class` hands a Durable Object class to the worker's `env`.

### WebSocket

The API follows workerd: `new WebSocketPair()`, `server.accept()` (or the hibernation API `ctx.acceptWebSocket(ws, tags)`,
whose events go to the object's `webSocketMessage` / `webSocketClose`, with `ctx.getWebSockets(tag)` and
`serializeAttachment`), `send`, `close`, `addEventListener`, and `new Response(null, { status: 101, webSocket: client })`.

The network layer (`src/ws.rs`) is one epoll reactor thread for the whole process (mio): it accepts, reads requests,
parses frames and writes, without blocking, and never runs JS. Requests and messages reach the event loop through
single-consumer streams that JS awaits (`net.accept(server)`, `net.recv(id)`); replies, upgrades and frames go back to
the reactor as commands. With no messages, the event loop sleeps.

`examples/chat.js` is a chat room: a message is written to the object's SQLite table, then sent to everyone in the room;
newcomers get the stored history first. `tests/chat.rs` stresses it: eight clients, two rounds of concurrent messages,
five seconds of silence in between (the server must not burn CPU), the same order seen by every client, then a restart on
the same storage replaying the history in order.

### A thousand Workers in one process

`--instances N` starts N Workers in one process, each with its own runtime (isolate, context, Durable Objects,
`<storage>/<i>`) and port, all on one [event loop set](event-loop.md) with `--threads` loop threads. `tests/workerds.rs`
runs a thousand chat rooms this way: every room talks at once for two rounds, five seconds of silence in between (the
whole process must not burn CPU), then a restart and every room's history replayed.

All instances share one `CodeCache` (the prelude and the Worker's modules compile once) and one epoll reactor thread.

## jumpheap

`samples/jumpheap`: resident j8 workers in separate processes whose **JS objects reference each other's heaps**.

Each process owns a paragc **public heap** (a memory file in `/dev/shm`); its worker (a workerd-style
`export default { fetch }`) makes JS matrices there, sends them to other workers over HTTP, and listens for theirs. What
arrives is an `object` event carrying a JS `Matrix`: the object is in the receiver's own isolate heap, and its rows are
`Float64Array`s **over the sender's heap**, not copies. Everything computed on it is plain JS.

```sh
cargo run --release -- examples/matrix.js --name wa --dir /dev/shm/jumpheap   # prints its port
```

| JS API | |
|---|---|
| `await heaps.matrix(rows)` | a new all-zero `rows` × 100000 `Matrix` in this process's heap |
| `matrix.data[i]` | row `i`: a `Float64Array` over public heap memory (this process's or another's) |
| `matrix.rows`, `matrix.cols`, `matrix.local` | the shape; whether it is in this process's heap |
| `await heaps.send(url, matrix, name, task)` | hand `matrix` to the worker at `url`, get its answer |
| `heaps.addEventListener('object', e => …)` | a matrix arrived: `e.object`, `e.name`, `e.task`; `e.respondWith(value)` answers the sender |

A received matrix holds an entry in the sender's heap, and its rows stay alive as long as the receiver holds any row's
buffer; after the sender dies, what it wrote into other heaps remains. `tests/three.rs` starts three workers that send
each other matrices, kills one mid-way, and checks the results are still right.
