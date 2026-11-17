// workerd's prelude: a minimal subset of the Fetch API (Headers / Request / Response / URL / URLSearchParams)
// and Durable Objects.
// The native part of storage (one KV per object) is `__j8_native.KvStore`, bound from Rust with j8binding.
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

  // ---- Durable Object ----

  class DurableObjectId {
    constructor(name) { this.name = name; }
    toString() { return this.name; }
    equals(other) { return other instanceof DurableObjectId && other.name === this.name; }
  }

  /** `state.storage`: values are stored as JSON in the native KV; the whole interface is Promise-based. */
  class DurableObjectStorage {
    constructor(kv) { this._kv = kv; }
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

  class DurableObjectState {
    constructor(id, storage) { this.id = id; this.storage = storage; }
    blockConcurrencyWhile(f) { return Promise.resolve().then(f); }
    waitUntil() {}
  }

  /** `env.X`: get an object by id (same id, same instance; each has its own storage). */
  class DurableObjectNamespace {
    constructor(cls, env) { this._cls = cls; this._env = env; this._live = new Map(); }
    idFromName(name) { return new DurableObjectId(String(name)); }
    idFromString(s) { return new DurableObjectId(String(s)); }
    newUniqueId() { return new DurableObjectId(native.KvStore.uniqueId()); }
    get(id) {
      const key = id.toString();
      let inst = this._live.get(key);
      if (!inst) {
        inst = new this._cls(new DurableObjectState(id, new DurableObjectStorage(new native.KvStore())), this._env);
        this._live.set(key, inst);
      }
      return {
        id,
        name: id.name,
        fetch: (input, init) => Promise.resolve(inst.fetch(input instanceof Request ? input : new Request(input, init))),
      };
    }
  }

  Object.assign(globalThis, { Headers, Request, Response, URL, URLSearchParams });

  // Entry points called from the host (Rust) side
  globalThis.__workerd = {
    worker: null,
    env: {},
    error: null,
    result: null,
    setup(ns, bindings) {
      this.worker = ns;
      for (const [name, cls] of bindings) {
        if (typeof ns[cls] !== 'function') throw new TypeError('the worker does not export a class named ' + cls);
        this.env[name] = new DurableObjectNamespace(ns[cls], this.env);
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
    // The result goes to Rust as "status line, one header per line, blank line, body"
    take() {
      const r = this.result || { status: 500, headers: [], body: 'the worker did not respond\n' };
      const head = r.headers.map(([k, v]) => k + ': ' + v + '\n').join('');
      return r.status + '\n' + head + '\n' + r.body;
    },
  };
})(globalThis.__j8_native);
