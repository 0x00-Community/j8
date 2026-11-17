// workerd's prelude: a minimal subset of the Fetch API (Headers / Request / Response / URL / URLSearchParams),
// WebSockets (WebSocketPair, after workerd's api::WebSocket) and Durable Objects (KV + SQL storage, WebSocket
// hibernation API).
// The native parts are bound from Rust: `__j8_native.KvStore` (one KV per object), `__j8_native.SqlStorage` (one
// SQLite database per object) and `__j8_native.Server` (the HTTP / WebSocket server on the event loop).
(function (native) {
  'use strict';

  class Headers {
    constructor(init) {
      this._map = new Map();
      if (init == null) return;
      const pairs = init instanceof Headers ? init._map : Array.isArray(init) ? init : Object.entries(init);
      for (const [k, v] of pairs) this.append(k, v);
    }
    get(k) { const v = this._map.get(String(k).toLowerCase()); return v === undefined ? null : v; }
    set(k, v) { this._map.set(String(k).toLowerCase(), String(v)); }
    append(k, v) {
      k = String(k).toLowerCase();
      const old = this._map.get(k);
      this._map.set(k, old === undefined ? String(v) : old + ', ' + v);
    }
    has(k) { return this._map.has(String(k).toLowerCase()); }
    delete(k) { this._map.delete(String(k).toLowerCase()); }
    forEach(f, self) { for (const [k, v] of this._map) f.call(self, v, k, this); }
    entries() { return this._map.entries(); }
    keys() { return this._map.keys(); }
    values() { return this._map.values(); }
    [Symbol.iterator]() { return this._map.entries(); }
  }

  class Body {
    text() { return Promise.resolve(this._body == null ? '' : this._body); }
    json() { return this.text().then(JSON.parse); }
    get bodyUsed() { return false; }
  }

  class Request extends Body {
    constructor(input, init = {}) {
      super();
      const from = input instanceof Request ? input : null;
      this.url = from ? from.url : String(input);
      this.method = String(init.method || (from && from.method) || 'GET').toUpperCase();
      this.headers = new Headers(init.headers || (from && from.headers));
      this._body = init.body != null ? String(init.body) : from ? from._body : null;
    }
    clone() { return new Request(this); }
  }

  class Response extends Body {
    constructor(body = null, init = {}) {
      super();
      this._body = body == null ? '' : String(body);
      this.status = init.status === undefined ? 200 : init.status;
      this.statusText = init.statusText || '';
      this.headers = new Headers(init.headers);
      this.webSocket = init.webSocket || null;
      if (this.status === 101 && !this.webSocket) throw new RangeError('a 101 response needs a webSocket');
    }
    get ok() { return this.status >= 200 && this.status < 300; }
    clone() { return new Response(this._body, this); }
    static json(value, init) {
      const r = new Response(JSON.stringify(value), init);
      if (!r.headers.has('content-type')) r.headers.set('content-type', 'application/json');
      return r;
    }
    static redirect(url, status = 302) { return new Response(null, { status, headers: { location: String(url) } }); }
  }

  class URLSearchParams {
    constructor(init = '') {
      this._pairs = [];
      let s = String(init);
      if (s.startsWith('?')) s = s.slice(1);
      for (const part of s.split('&')) {
        if (!part) continue;
        const i = part.indexOf('=');
        const dec = (x) => decodeURIComponent(x.replace(/\+/g, ' '));
        this._pairs.push(i < 0 ? [dec(part), ''] : [dec(part.slice(0, i)), dec(part.slice(i + 1))]);
      }
    }
    get(k) { const e = this._pairs.find((p) => p[0] === k); return e ? e[1] : null; }
    getAll(k) { return this._pairs.filter((p) => p[0] === k).map((p) => p[1]); }
    has(k) { return this._pairs.some((p) => p[0] === k); }
    set(k, v) { this.delete(k); this._pairs.push([String(k), String(v)]); }
    append(k, v) { this._pairs.push([String(k), String(v)]); }
    delete(k) { this._pairs = this._pairs.filter((p) => p[0] !== k); }
    toString() { return this._pairs.map(([k, v]) => encodeURIComponent(k) + '=' + encodeURIComponent(v)).join('&'); }
    [Symbol.iterator]() { return this._pairs[Symbol.iterator](); }
  }

  class URL {
    constructor(url, base) {
      let u = String(url);
      if (base !== undefined && !/^[a-z][a-z0-9+.-]*:/i.test(u)) {
        const b = new URL(base);
        u = u.startsWith('/') ? b.origin + u : b.origin + b.pathname.replace(/[^/]*$/, '') + u;
      }
      const m = /^([a-z][a-z0-9+.-]*:)\/\/([^/?#]*)([^?#]*)(\?[^#]*)?(#.*)?$/i.exec(u);
      if (!m) throw new TypeError('Invalid URL: ' + u);
      this.protocol = m[1].toLowerCase();
      this.host = m[2];
      [this.hostname, this.port = ''] = this.host.split(':');
      this.pathname = m[3] || '/';
      this.search = m[4] && m[4] !== '?' ? m[4] : '';
      this.hash = m[5] || '';
      this.searchParams = new URLSearchParams(this.search);
    }
    get origin() { return this.protocol + '//' + this.host; }
    get href() { return this.origin + this.pathname + this.search + this.hash; }
    toString() { return this.href; }
    toJSON() { return this.href; }
  }

  // ---- WebSocket ----
  //
  // A WebSocketPair is two linked ends: what one end sends, the other end receives. The end a Worker returns in
  // `new Response(null, { status: 101, webSocket: client })` is coupled to the network connection, so what the
  // accepted end sends goes to the client and what the client sends arrives at the accepted end. Before accept(),
  // received messages wait in a queue (as in workerd).

  const INTERNAL = Symbol('internal');
  const CONNECTING = 0, OPEN = 1, CLOSING = 2, CLOSED = 3;

  class WebSocket {
    constructor(token) {
      if (token !== INTERNAL) throw new TypeError('outbound WebSocket connections are not supported; use WebSocketPair');
      this._peer = null;
      this._native = null;       // { server, id } once coupled to a network connection
      this._accepted = false;
      this._queue = [];          // events received before accept()
      this._listeners = new Map();
      this._closedOutgoing = false;
      this._closedIncoming = false;
      this._attachment = undefined;
      this.readyState = CONNECTING;
      this.url = null;
      this.protocol = '';
      this.extensions = '';
      this.binaryType = 'arraybuffer';
      this.onmessage = this.onclose = this.onerror = this.onopen = null;
    }

    accept() {
      if (this._native) throw new TypeError("Can't accept() a WebSocket that was already used in a response");
      if (this._accepted) return;
      this._accepted = true;
      if (this.readyState === CONNECTING) this.readyState = OPEN;
      const queued = this._queue;
      this._queue = [];
      if (queued.length) Promise.resolve().then(() => queued.forEach((e) => this._dispatch(e)));
    }

    send(message) {
      if (!this._accepted) throw new TypeError('You must call accept() on this WebSocket before sending messages.');
      if (this._closedOutgoing) throw new TypeError("Can't call WebSocket send() after close().");
      if (typeof message !== 'string' && !(message instanceof ArrayBuffer) && !ArrayBuffer.isView(message)) message = String(message);
      this._peer._receive({ type: 'message', data: message });
    }

    close(code = 1000, reason = '') {
      if (!this._accepted) throw new TypeError('You must call accept() on this WebSocket before closing it.');
      if (this._closedOutgoing) return;
      this._closedOutgoing = true;
      this.readyState = this._closedIncoming ? CLOSED : CLOSING;
      this._peer._receive({ type: 'close', code, reason: String(reason), wasClean: true });
    }

    get bufferedAmount() {
      const n = this._peer && this._peer._native;
      return n ? n.server.bufferedAmount(n.id) : 0;
    }

    serializeAttachment(v) { this._attachment = v === undefined ? undefined : JSON.stringify(v); }
    deserializeAttachment() { return this._attachment === undefined ? null : JSON.parse(this._attachment); }

    addEventListener(type, f) {
      if (typeof f !== 'function' && !(f && typeof f.handleEvent === 'function')) return;
      const list = this._listeners.get(type) || [];
      if (!list.includes(f)) list.push(f);
      this._listeners.set(type, list);
    }

    removeEventListener(type, f) {
      const list = this._listeners.get(type);
      if (list) this._listeners.set(type, list.filter((g) => g !== f));
    }

    // An event arriving at this end: to the network when coupled, to listeners once accepted, queued before that
    _receive(e) {
      if (this._native) return this._toNetwork(e);
      if (!this._accepted) return void this._queue.push(e);
      this._dispatch(e);
    }

    _toNetwork(e) {
      const { server, id } = this._native;
      if (e.type === 'close') return server.close(id, e.code, e.reason);
      if (typeof e.data === 'string') return server.sendText(id, e.data);
      server.sendBinary(id, e.data instanceof ArrayBuffer ? new Uint8Array(e.data) : e.data);
    }

    _dispatch(e) {
      if (e.type === 'close') {
        this._closedIncoming = true;
        this.readyState = this._closedOutgoing ? CLOSED : CLOSING;
      }
      const event = Object.assign({ target: this, currentTarget: this }, e);
      const handlers = [...(this._listeners.get(e.type) || [])];
      const prop = this['on' + e.type];
      if (typeof prop === 'function') handlers.push(prop);
      for (const h of handlers) {
        try {
          const r = typeof h === 'function' ? h.call(this, event) : h.handleEvent(event);
          if (r && typeof r.then === 'function') r.then(undefined, report);
        } catch (err) {
          report(err);
        }
      }
    }
  }
  for (const [k, v] of Object.entries({ CONNECTING, OPEN, CLOSING, CLOSED })) {
    WebSocket[k] = v;
    WebSocket['READY_STATE_' + k] = v;
  }

  class WebSocketPair {
    constructor() {
      const a = new WebSocket(INTERNAL), b = new WebSocket(INTERNAL);
      a._peer = b;
      b._peer = a;
      Object.defineProperty(this, 0, { value: a, enumerable: true });
      Object.defineProperty(this, 1, { value: b, enumerable: true });
    }
    *[Symbol.iterator]() { yield this[0]; yield this[1]; }
  }

  function report(err) {
    console.log('Uncaught ' + ((err && err.stack) || err));
  }

  /** Couples `end` (returned in a 101 response) to network connection `id`: what the client sends arrives at the
   * other end; a Close or a dropped connection ends it */
  async function couple(end, server, id) {
    end._native = { server, id };
    end.readyState = OPEN;
    // What the accepted end sent before the response went out (a history, a greeting) goes first
    const queued = end._queue;
    end._queue = [];
    queued.forEach((e) => end._toNetwork(e));
    const peer = end._peer;
    try {
      for (;;) {
        const m = await server.recv(id);
        if (m == null) {
          if (!peer._closedIncoming) peer._receive({ type: 'close', code: 1006, reason: '', wasClean: false });
          break;
        }
        if (typeof m === 'string') peer._receive({ type: 'message', data: m });
        else if (m instanceof Uint8Array) peer._receive({ type: 'message', data: m.buffer });
        else {
          peer._receive({ type: 'close', code: m.close, reason: m.reason, wasClean: true });
          break;
        }
      }
    } finally {
      end.readyState = CLOSED;
      server.release(id);
    }
  }

  // ---- Durable Object ----

  class DurableObjectId {
    constructor(name) { this.name = name; }
    toString() { return this.name; }
    equals(other) { return other instanceof DurableObjectId && other.name === this.name; }
  }

  /** `state.storage`: values are stored as JSON in the native KV; the whole interface is Promise-based. */
  class DurableObjectStorage {
    constructor(kv, sqlPath) { this._kv = kv; this._sqlPath = sqlPath; this._sql = null; }
    get sql() {
      if (!this._sql) this._sql = new SqlStorage(new native.SqlStorage(this._sqlPath));
      return this._sql;
    }
    get(key) {
      if (Array.isArray(key)) return Promise.all(key.map((k) => this.get(k))).then((vs) => new Map(key.map((k, i) => [k, vs[i]])));
      const v = this._kv.get(String(key));
      return Promise.resolve(v === undefined ? undefined : JSON.parse(v));
    }
    put(key, value) {
      if (typeof key === 'object' && key !== null) {
        for (const [k, v] of Object.entries(key)) this._kv.put(k, JSON.stringify(v));
      } else {
        this._kv.put(String(key), JSON.stringify(value));
      }
      return Promise.resolve();
    }
    delete(key) {
      if (Array.isArray(key)) return Promise.resolve(key.filter((k) => this._kv.delete(String(k))).length);
      return Promise.resolve(this._kv.delete(String(key)));
    }
    list() {
      const m = new Map();
      for (const k of this._kv.keys()) m.set(k, JSON.parse(this._kv.get(k)));
      return Promise.resolve(m);
    }
    deleteAll() {
      for (const k of this._kv.keys()) this._kv.delete(k);
      return Promise.resolve();
    }
  }

  /** The rows of one `sql.exec()`: iterate for row objects, or `toArray()` / `one()` / `raw()` */
  class SqlStorageCursor {
    constructor(r) {
      this.columnNames = r.columns;
      this.rowsRead = r.rowsRead;
      this.rowsWritten = r.rowsWritten;
      this._rows = r.rows;
      this._next = 0;
    }
    _object(row) {
      const o = {};
      this.columnNames.forEach((c, i) => { o[c] = row[i]; });
      return o;
    }
    next() {
      if (this._next >= this._rows.length) return { done: true, value: undefined };
      return { done: false, value: this._object(this._rows[this._next++]) };
    }
    [Symbol.iterator]() { return this; }
    toArray() { return [...this]; }
    one() {
      const rest = this._rows.length - this._next;
      if (rest !== 1) throw new Error('Expected exactly one result from SQL query, but got ' + (rest ? 'more than one' : 'no') + ' results.');
      return this.next().value;
    }
    *raw() { while (this._next < this._rows.length) yield this._rows[this._next++]; }
  }

  /** `ctx.storage.sql`: synchronous SQLite, one database per object */
  class SqlStorage {
    constructor(db) { this._db = db; }
    exec(query, ...bindings) { return new SqlStorageCursor(JSON.parse(this._db.exec(String(query), JSON.stringify(bindings)))); }
    get databaseSize() { return this._db.databaseSize; }
  }

  class DurableObjectState {
    constructor(id, storage) { this.id = id; this.storage = storage; this._sockets = new Set(); this._object = null; }
    blockConcurrencyWhile(f) { return Promise.resolve().then(f); }
    waitUntil() {}

    /** The hibernation API: accepts `ws` and delivers its events to the object's `webSocketMessage` /
     * `webSocketClose` / `webSocketError` */
    acceptWebSocket(ws, tags = []) {
      ws.accept();
      ws._tags = tags.map(String);
      this._sockets.add(ws);
      const call = (name, ...args) => {
        const f = this._object && this._object[name];
        if (typeof f === 'function') return f.apply(this._object, args);
      };
      ws.addEventListener('message', (e) => call('webSocketMessage', ws, e.data));
      ws.addEventListener('close', (e) => {
        this._sockets.delete(ws);
        return call('webSocketClose', ws, e.code, e.reason, e.wasClean);
      });
      ws.addEventListener('error', (e) => call('webSocketError', ws, e.error));
    }

    getWebSockets(tag) {
      return [...this._sockets].filter((ws) => tag === undefined || ws._tags.includes(String(tag)));
    }

    getTags(ws) { return ws._tags || []; }
  }

  /** `env.X`: get an object by id (same id, same instance; each has its own storage). */
  class DurableObjectNamespace {
    constructor(name, cls, env, storageDir) { this._name = name; this._cls = cls; this._env = env; this._dir = storageDir; this._live = new Map(); }
    idFromName(name) { return new DurableObjectId(String(name)); }
    idFromString(s) { return new DurableObjectId(String(s)); }
    newUniqueId() { return new DurableObjectId(native.KvStore.uniqueId()); }
    get(id) {
      const key = id.toString();
      let inst = this._live.get(key);
      if (!inst) {
        const sqlPath = this._dir ? this._dir + '/' + encodeURIComponent(this._name + '-' + key) + '.sqlite' : '';
        const state = new DurableObjectState(id, new DurableObjectStorage(new native.KvStore(), sqlPath));
        inst = new this._cls(state, this._env);
        state._object = inst;
        this._live.set(key, inst);
      }
      return {
        id,
        name: id.name,
        fetch: (input, init) => Promise.resolve(inst.fetch(input instanceof Request ? input : new Request(input, init))),
      };
    }
  }

  Object.assign(globalThis, { Headers, Request, Response, URL, URLSearchParams, WebSocket, WebSocketPair });

  // Entry points called from the host (Rust) side
  globalThis.__workerd = {
    worker: null,
    env: {},
    error: null,
    result: null,
    setup(ns, bindings, storageDir) {
      this.worker = ns;
      for (const [name, cls] of bindings) {
        if (typeof ns[cls] !== 'function') throw new TypeError('the worker does not export a class named ' + cls);
        this.env[name] = new DurableObjectNamespace(name, ns[cls], this.env, storageDir);
      }
      if (!ns.default || typeof ns.default.fetch !== 'function') throw new TypeError('the worker has no default export with fetch()');
    },
    dispatch(method, url, headers, body) {
      const req = new Request(url, { method, headers, body: body === '' ? null : body });
      const ctx = { waitUntil() {}, passThroughOnException() {} };
      this.result = null;
      Promise.resolve()
        .then(() => this.worker.default.fetch(req, this.env, ctx))
        .then((r) => r.text().then((b) => { this.result = { status: r.status, headers: [...r.headers], body: b }; }))
        .catch((e) => { this.result = { status: 500, headers: [['content-type', 'text/plain']], body: String((e && e.stack) || e) + '\n' }; });
    },
    // The server: every request goes to the Worker's fetch; a 101 response with a webSocket upgrades the connection
    // and couples it to that end. Requests are handled concurrently: the loop asks for the next one at once.
    serve(port) {
      const server = new native.Server(port);
      const loop = async () => {
        for (;;) {
          const r = await server.accept();
          if (r == null) return;
          this.handle(server, r);
        }
      };
      loop().catch(report);
    },
    handle(server, r) {
      const req = new Request('http://localhost' + r.path, { method: r.method, headers: r.headers, body: r.body === '' ? null : r.body });
      const ctx = { waitUntil() {}, passThroughOnException() {} };
      const fail = (e) => server.respond(r.id, 500, [['content-type', 'text/plain']], String((e && e.stack) || e) + '\n');
      Promise.resolve()
        .then(() => this.worker.default.fetch(req, this.env, ctx))
        .then((res) => {
          if (res.status === 101) {
            server.upgrade(r.id);
            couple(res.webSocket, server, r.id).catch(report);
            return;
          }
          return res.text().then((body) => server.respond(r.id, res.status, [...res.headers], body));
        })
        .catch((e) => {
          try { fail(e); } catch (e2) { report(e); }
        });
    },
    // The result goes to Rust as "status line, one header per line, blank line, body"
    take() {
      const r = this.result || { status: 500, headers: [], body: 'the worker did not respond\n' };
      const head = r.headers.map(([k, v]) => k + ': ' + v + '\n').join('');
      return r.status + '\n' + head + '\n' + r.body;
    },
  };
})(globalThis.__j8_native);
