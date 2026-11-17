// Fetch: Headers, Blob, Request, Response, fetch (http: over native.http, data: URLs; redirects, AbortSignal).
(function (native) {
  'use strict';
  const { http, codec } = native;
  const encoder = new TextEncoder();
  const decoder = new TextDecoder();
  const TOKEN = /^[!#$%&'*+.^_`|~0-9A-Za-z-]+$/;
  const STATUS = { 200: 'OK', 201: 'Created', 204: 'No Content', 301: 'Moved Permanently', 302: 'Found', 303: 'See Other', 304: 'Not Modified', 307: 'Temporary Redirect', 308: 'Permanent Redirect', 400: 'Bad Request', 401: 'Unauthorized', 403: 'Forbidden', 404: 'Not Found', 405: 'Method Not Allowed', 409: 'Conflict', 500: 'Internal Server Error', 502: 'Bad Gateway', 503: 'Service Unavailable' };

  const name = (k) => {
    k = String(k);
    if (!TOKEN.test(k)) throw new TypeError('Invalid header name: ' + k);
    return k.toLowerCase();
  };
  const value = (v) => String(v).replace(/^[\t\n\r ]+|[\t\n\r ]+$/g, '');

  class Headers {
    #map = new Map();
    #cookies = [];
    constructor(init) {
      if (init == null) return;
      if (init instanceof Headers) { init.forEach((v, k) => this.append(k, v)); return; }
      if (typeof init !== 'object') throw new TypeError('Headers init must be an object or iterable');
      if (typeof init[Symbol.iterator] === 'function') {
        for (const pair of init) {
          const p = [...pair];
          if (p.length !== 2) throw new TypeError('Header pairs must have exactly two items');
          this.append(p[0], p[1]);
        }
      } else for (const k of Object.keys(init)) this.append(k, init[k]);
    }
    append(k, v) {
      k = name(k); v = value(v);
      if (k === 'set-cookie') this.#cookies.push(v);
      const old = this.#map.get(k);
      this.#map.set(k, old === undefined ? v : old + ', ' + v);
    }
    set(k, v) { k = name(k); v = value(v); if (k === 'set-cookie') this.#cookies = [v]; this.#map.set(k, v); }
    get(k) { const v = this.#map.get(name(k)); return v === undefined ? null : v; }
    getSetCookie() { return this.#cookies.slice(); }
    has(k) { return this.#map.has(name(k)); }
    delete(k) { k = name(k); if (k === 'set-cookie') this.#cookies = []; this.#map.delete(k); }
    forEach(f, self) { for (const [k, v] of this.entries()) f.call(self, v, k, this); }
    *entries() {
      for (const k of [...this.#map.keys()].sort()) {
        if (k === 'set-cookie') for (const c of this.#cookies) yield [k, c];
        else yield [k, this.#map.get(k)];
      }
    }
    keys() { return Array.from(this.entries(), (e) => e[0])[Symbol.iterator](); }
    values() { return Array.from(this.entries(), (e) => e[1])[Symbol.iterator](); }
    [Symbol.iterator]() { return this.entries(); }
    get [Symbol.toStringTag]() { return 'Headers'; }
  }

  /** Bytes plus a content type, from any BodyInit */
  function extract(body) {
    if (body == null) return { bytes: null, type: null };
    if (typeof body === 'string') return { bytes: encoder.encode(body), type: 'text/plain;charset=UTF-8' };
    if (body instanceof Blob) return { bytes: Blob.bytes(body), type: body.type || null };
    if (body instanceof URLSearchParams) return { bytes: encoder.encode(body.toString()), type: 'application/x-www-form-urlencoded;charset=UTF-8' };
    if (body instanceof ArrayBuffer) return { bytes: new Uint8Array(body.slice(0)), type: null };
    if (ArrayBuffer.isView(body)) return { bytes: new Uint8Array(body.buffer.slice(body.byteOffset, body.byteOffset + body.byteLength)), type: null };
    return { bytes: encoder.encode(String(body)), type: 'text/plain;charset=UTF-8' };
  }

  class Blob {
    #bytes;
    #type;
    constructor(parts = [], options = {}) {
      const chunks = [];
      for (const p of parts) chunks.push(p instanceof Blob ? p.#bytes : typeof p === 'string' ? encoder.encode(p) : extract(p).bytes);
      const total = chunks.reduce((n, c) => n + c.length, 0);
      this.#bytes = new Uint8Array(total);
      let at = 0;
      for (const c of chunks) { this.#bytes.set(c, at); at += c.length; }
      this.#type = options.type ? String(options.type).toLowerCase() : '';
    }
    static bytes(b) { return b.#bytes; }
    get size() { return this.#bytes.length; }
    get type() { return this.#type; }
    slice(start = 0, end = this.size, type = '') {
      const b = new Blob([], { type });
      b.#bytes = this.#bytes.slice(start, end);
      return b;
    }
    text() { return Promise.resolve(decoder.decode(this.#bytes)); }
    arrayBuffer() { return Promise.resolve(this.#bytes.slice().buffer); }
    bytes() { return Promise.resolve(this.#bytes.slice()); }
    get [Symbol.toStringTag]() { return 'Blob'; }
  }

  class File extends Blob {
    constructor(parts, fileName, options = {}) {
      super(parts, options);
      this.name = String(fileName);
      this.lastModified = options.lastModified === undefined ? Date.now() : Number(options.lastModified);
    }
    get [Symbol.toStringTag]() { return 'File'; }
  }

  const bodies = new WeakMap();

  class Body {
    #used = false;
    static init(self, body, headers) {
      const { bytes, type } = extract(body);
      bodies.set(self, bytes);
      if (type && !headers.has('content-type')) headers.set('content-type', type);
    }
    get body() { return bodies.get(this) ? { locked: this.#used } : null; }
    get bodyUsed() { return this.#used; }
    #consume() {
      if (this.#used) return Promise.reject(new TypeError('Body is unusable: Body has already been read'));
      this.#used = true;
      return Promise.resolve(bodies.get(this) || new Uint8Array(0));
    }
    arrayBuffer() { return this.#consume().then((b) => b.slice().buffer); }
    bytes() { return this.#consume().then((b) => b.slice()); }
    text() { return this.#consume().then((b) => decoder.decode(b)); }
    json() { return this.text().then(JSON.parse); }
    blob() { return this.#consume().then((b) => new Blob([b], { type: this.headers.get('content-type') || '' })); }
  }

  class Request extends Body {
    constructor(input, init = {}) {
      super();
      const from = input instanceof Request ? input : null;
      this.url = from ? from.url : new URL(String(input)).href;
      this.method = String(init.method || (from ? from.method : 'GET')).toUpperCase();
      this.headers = new Headers(init.headers || (from ? from.headers : undefined));
      this.redirect = init.redirect || (from ? from.redirect : 'follow');
      this.signal = init.signal || (from ? from.signal : new AbortController().signal);
      this.cache = 'default';
      this.credentials = 'same-origin';
      this.mode = 'cors';
      const body = init.body !== undefined ? init.body : from ? bodies.get(from) : null;
      if (body != null && (this.method === 'GET' || this.method === 'HEAD')) throw new TypeError('Request with GET/HEAD method cannot have body.');
      Body.init(this, body, this.headers);
    }
    clone() { return new Request(this); }
    get [Symbol.toStringTag]() { return 'Request'; }
  }

  class Response extends Body {
    constructor(body = null, init = {}) {
      super();
      const status = init.status === undefined ? 200 : Number(init.status);
      if (!(status >= 200 && status <= 599)) throw new RangeError('The status provided (' + status + ') is outside the range [200, 599].');
      this.status = status;
      this.statusText = init.statusText === undefined ? '' : String(init.statusText);
      this.headers = new Headers(init.headers);
      this.type = 'default';
      this.url = '';
      this.redirected = false;
      Body.init(this, body, this.headers);
    }
    get ok() { return this.status >= 200 && this.status < 300; }
    clone() {
      if (this.bodyUsed) throw new TypeError('Response body is already used');
      const r = new Response(bodies.get(this), this);
      Object.assign(r, { type: this.type, url: this.url, redirected: this.redirected });
      return r;
    }
    static json(data, init = {}) {
      const r = new Response(JSON.stringify(data), init);
      r.headers.set('content-type', 'application/json');
      return r;
    }
    static redirect(url, status = 302) {
      if (![301, 302, 303, 307, 308].includes(status)) throw new RangeError('Invalid status code');
      return new Response(null, { status, headers: { location: new URL(url).href } });
    }
    static error() { const r = new Response(null, { status: 200 }); r.status = 0; r.type = 'error'; return r; }
    get [Symbol.toStringTag]() { return 'Response'; }
  }

  /** Splits a raw HTTP/1.1 response into status, headers and the (de-chunked) body */
  function parseResponse(raw) {
    let end = -1;
    for (let i = 0; i + 3 < raw.length; i++) if (raw[i] === 13 && raw[i + 1] === 10 && raw[i + 2] === 13 && raw[i + 3] === 10) { end = i; break; }
    if (end < 0) throw new TypeError('fetch failed: malformed response');
    const lines = new TextDecoder('latin1').decode(raw.subarray(0, end)).split('\r\n');
    const m = /^HTTP\/\d\.\d (\d{3}) ?(.*)$/.exec(lines[0]);
    if (!m) throw new TypeError('fetch failed: malformed status line');
    const headers = new Headers();
    for (const l of lines.slice(1)) { const c = l.indexOf(':'); if (c > 0) headers.append(l.slice(0, c).trim(), l.slice(c + 1).trim()); }
    let body = raw.subarray(end + 4);
    if (/chunked/i.test(headers.get('transfer-encoding') || '')) {
      const parts = [];
      let at = 0;
      for (;;) {
        let eol = at;
        while (eol + 1 < body.length && !(body[eol] === 13 && body[eol + 1] === 10)) eol++;
        const size = parseInt(new TextDecoder('latin1').decode(body.subarray(at, eol)), 16);
        if (!(size > 0)) break;
        parts.push(body.subarray(eol + 2, eol + 2 + size));
        at = eol + 4 + size;
      }
      body = new Uint8Array(parts.reduce((n, p) => n + p.length, 0));
      let o = 0;
      for (const p of parts) { body.set(p, o); o += p.length; }
    } else if (headers.has('content-length')) body = body.subarray(0, Number(headers.get('content-length')));
    return { status: Number(m[1]), statusText: m[2], headers, body };
  }

  function dataUrl(url) {
    const m = /^data:([^,]*?)(;base64)?,(.*)$/s.exec(url);
    if (!m) throw new TypeError('fetch failed: invalid data: URL');
    const bytes = m[2] ? codec.base64Decode(decodeURIComponent(m[3])) : encoder.encode(decodeURIComponent(m[3]));
    return new Response(bytes, { headers: { 'content-type': m[1] || 'text/plain;charset=US-ASCII' } });
  }

  async function fetch(input, init) {
    let req = new Request(input, init);
    const signal = init && init.signal ? init.signal : input instanceof Request ? input.signal : null;
    let body = bodies.get(req);
    for (let hops = 0; ; hops++) {
      if (signal && signal.aborted) throw signal.reason;
      const url = new URL(req.url);
      if (url.protocol === 'data:') return dataUrl(req.url);
      if (url.protocol !== 'http:') throw new TypeError('fetch failed: ' + url.protocol + ' is not supported (dune speaks plain http only)');
      const headers = new Headers(req.headers);
      headers.set('host', url.host);
      headers.set('connection', 'close');
      if (!headers.has('accept')) headers.set('accept', '*/*');
      if (!headers.has('user-agent')) headers.set('user-agent', 'dune');
      if (body) headers.set('content-length', String(body.length));
      let head = req.method + ' ' + url.pathname + url.search + ' HTTP/1.1\r\n';
      for (const [k, v] of headers) head += k + ': ' + v + '\r\n';
      let raw;
      try {
        raw = await http.exchange(url.hostname, Number(url.port || 80), head + '\r\n', body || new Uint8Array(0));
      } catch (e) {
        throw new TypeError('fetch failed', { cause: e });
      }
      if (signal && signal.aborted) throw signal.reason;
      const r = parseResponse(raw);
      const location = r.headers.get('location');
      if ([301, 302, 303, 307, 308].includes(r.status) && location && req.redirect !== 'manual') {
        if (req.redirect === 'error') throw new TypeError('fetch failed: redirect mode is "error"');
        if (hops >= 20) throw new TypeError('fetch failed: too many redirects');
        const keep = r.status === 307 || r.status === 308;
        if (!keep) body = null;
        req = new Request(new URL(location, url).href, { method: keep ? req.method : req.method === 'HEAD' ? 'HEAD' : 'GET', headers: req.headers, redirect: req.redirect, body });
        continue;
      }
      const res = new Response(req.method === 'HEAD' || r.status === 204 || r.status === 304 ? null : r.body, { status: r.status < 200 ? 200 : r.status, statusText: r.statusText || STATUS[r.status] || '', headers: r.headers });
      Object.assign(res, { url: url.href, redirected: hops > 0, type: 'basic' });
      return res;
    }
  }

  for (const [n, v] of Object.entries({ Headers, Blob, File, Request, Response, fetch })) {
    Object.defineProperty(globalThis, n, { value: v, writable: true, configurable: true });
  }
})(globalThis.__j8_native);
