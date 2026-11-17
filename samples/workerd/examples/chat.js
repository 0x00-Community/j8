// A chat room: every WebSocket that connects to /room?name=<who> joins one Durable Object. Each message is stored in
// the object's SQLite storage, then broadcast to everyone in the room (the sender too), so all clients see one order.
// A newcomer first gets the stored history.
//
//   workerd examples/chat.js --bind ROOM=ChatRoom --port 8787 --storage .chat
//
// Wire format, server to client: {"id":n,"name":"…","text":"…"} per message, with "history":true for replayed ones.
export class ChatRoom {
  constructor(ctx, env) {
    this.ctx = ctx;
    this.env = env;
    ctx.storage.sql.exec(`CREATE TABLE IF NOT EXISTS messages (
      id INTEGER PRIMARY KEY AUTOINCREMENT,
      name TEXT NOT NULL,
      text TEXT NOT NULL
    )`);
  }

  async fetch(request) {
    if (request.headers.get('upgrade') !== 'websocket') {
      const count = this.ctx.storage.sql.exec('SELECT count(*) AS n FROM messages').one().n;
      return Response.json({ messages: count, online: this.ctx.getWebSockets().length });
    }
    const name = new URL(request.url).searchParams.get('name') || 'anonymous';
    const [client, server] = Object.values(new WebSocketPair());
    this.ctx.acceptWebSocket(server, [name]);
    server.serializeAttachment({ name });
    for (const row of this.ctx.storage.sql.exec('SELECT id, name, text FROM messages ORDER BY id')) {
      server.send(JSON.stringify({ ...row, history: true }));
    }
    return new Response(null, { status: 101, webSocket: client });
  }

  webSocketMessage(ws, message) {
    const { name } = ws.deserializeAttachment();
    const text = typeof message === 'string' ? message : `[binary ${message.byteLength} bytes]`;
    const { id } = this.ctx.storage.sql.exec('INSERT INTO messages (name, text) VALUES (?, ?) RETURNING id', name, text).one();
    const out = JSON.stringify({ id, name, text });
    for (const peer of this.ctx.getWebSockets()) {
      try {
        peer.send(out);
      } catch (e) {
        // A peer that is closing: its close event takes it out of the room
      }
    }
  }

  webSocketClose(ws, code, reason) {
    try {
      ws.close(code === 1005 ? 1000 : code, reason);
    } catch (e) {
      // already closed
    }
  }
}

export default {
  async fetch(request, env) {
    const url = new URL(request.url);
    if (url.pathname !== '/room') return new Response('chat: connect a WebSocket to /room?name=<who>\n', { status: 404 });
    return env.ROOM.get(env.ROOM.idFromName('lobby')).fetch(request);
  },
};
