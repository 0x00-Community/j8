# workerd

**English** · [简体中文](README.zh-CN.md)

A minimal host that loads Cloudflare Workers (ES modules with `export default { fetch }`) and Durable Objects with
[j8](https://github.com/0x00-Community/j8) (a sample host).

```sh
cargo run -- examples/counter.js --bind COUNTER=Counter --request "GET /c/inc?name=a"
cargo run -- examples/counter.js --bind COUNTER=Counter --port 8787   # an HTTP server
cargo run -- examples/chat.js --bind ROOM=ChatRoom --port 8787 --storage .chat   # a WebSocket chat room
```

- The Fetch API subset, WebSockets and Durable Objects are JavaScript in `src/prelude.js`
- Each Durable Object has key-value storage (the Rust `KvStore`) and SQL storage (`ctx.storage.sql`, one SQLite file
  per object under `--storage`, in memory without it; `src/sql.rs`)
- `--bind NAME=Class` exposes a Durable Object class to the worker's `env`
- A package of its own (not part of the j8 workspace), built against the published `j8` crate

## WebSockets

The API follows workerd's: `new WebSocketPair()`, `server.accept()` (or the hibernation API
`ctx.acceptWebSocket(ws, tags)` with `webSocketMessage` / `webSocketClose` on the object, `ctx.getWebSockets(tag)`,
`serializeAttachment`), `send`, `close`, `addEventListener`, and `new Response(null, { status: 101, webSocket: client })`.
Messages the accepted end sends before the response goes out are queued and go first.

The network (`src/ws.rs`) is one epoll reactor thread (mio) for every socket of the process: it accepts, reads
requests, parses frames and writes, non-blocking, and never runs JS. Requests and messages reach the event loops
through single-consumer streams that JS awaits (`net.accept(server)`, `net.recv(id)`); answers, upgrades and frames go
back to the reactor as commands. Each connection follows workerd's `api::WebSocket::Native`: a request waiting for its
answer or upgrade, then a WebSocket with its close flags and an outgoing buffer. The event loops sleep while nothing
arrives.

`examples/chat.js` is a chat room: messages go into the object's SQLite table, then to everyone in the room, and a
newcomer first gets the stored history. `tests/chat.rs` runs it under load: eight clients on one port, two rounds of
concurrent messages with five silent seconds between them (the server must use no CPU, then wake for the next
message), every client seeing the same order, and a restart on the same storage that replays the history.

```sh
cargo test --test chat
```

## Many Workers in one process

`--instances N` serves N copies of the Worker from one process: each its own runtime (isolate, context, Durable
Objects, `<storage>/<i>`), on its own port (`--port 0`: a free one each, printed by index), all on one event loop set
with `--threads` loop threads (default: one per CPU). `tests/workerds.rs` runs a thousand chat rooms that way, two
clients each, two rounds in every room at once with five silent seconds between them (the whole process must use no
CPU), then a restart replaying every room's history:

```sh
cargo test --release --test workerds -- --nocapture
```

A Durable Object's SQLite file is created off the event loop's thread before the object is constructed (creating
one syncs the disk), so a thousand rooms opening at once do not hold up the loop threads.

## License

MIT, as part of [j8](https://github.com/0x00-Community/j8).
