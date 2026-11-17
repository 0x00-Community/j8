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

The server (`src/ws.rs`) runs on j8's event loop. Each connection's network side is laid out like workerd's
`api::WebSocket::Native`: a state (awaiting the response or the upgrade, then accepted), the close flags, and an
outgoing queue a pump thread writes. A thread per connection reads frames, and hands each message only to a promise JS
is waiting on (`server.recv(id)`), so a connection is read no further than the script has asked; the event loop
sleeps while nothing arrives.

`examples/chat.js` is a chat room: messages go into the object's SQLite table, then to everyone in the room, and a
newcomer first gets the stored history. `tests/chat.rs` runs it under load: eight clients on one port, two rounds of
concurrent messages with five silent seconds between them (the server must use no CPU, then wake for the next
message), every client seeing the same order, and a restart on the same storage that replays the history.

```sh
cargo test --test chat
```

## License

MIT, as part of [j8](https://github.com/0x00-Community/j8).
