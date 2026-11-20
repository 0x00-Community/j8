// URL and URLSearchParams (WHATWG URL, the common subset: special and opaque schemes, relative resolution, dot
// segments, default ports, percent-encoding, live searchParams).
(function () {
  'use strict';
  const SPECIAL = { 'http:': '80', 'https:': '443', 'ws:': '80', 'wss:': '443', 'ftp:': '21', 'file:': '' };
  const PATH_SET = /[\x00-\x20"#<>?`{}\x7f-￿]/g;
  const QUERY_SET = /[\x00-\x20"#<>\x7f-￿]/g;
  const FRAGMENT_SET = /[\x00-\x20"<>`\x7f-￿]/g;
  const USER_SET = /[\x00-\x20"#<>?`{}/:;=@[\\\]^|\x7f-￿]/g;
  const enc = (set) => (s) => s.replace(set, (c) => encodeURIComponent(c).replace(/%25/g, '%'));

  function removeDots(path) {
    const out = [];
    const segs = path.split('/');
    for (let i = 0; i < segs.length; i++) {
      const s = segs[i];
      const last = i === segs.length - 1;
      if (s === '..' || s.toLowerCase() === '%2e%2e') { if (out.length > 1) out.pop(); if (last) out.push(''); } else if (s === '.' || s.toLowerCase() === '%2e') { if (last) out.push(''); } else out.push(s);
    }
    return out.join('/');
  }

  /** Parses `input` (against `base`, a parsed record) into a record, or null */
  function parse(input, base) {
    let s = String(input).replace(/^[\x00-\x20]+|[\x00-\x20]+$/g, '').replace(/[\t\n\r]/g, '');
    const r = { protocol: '', username: '', password: '', hostname: '', port: '', pathname: '', search: '', hash: '', opaque: false };
    const h = s.indexOf('#');
    if (h >= 0) { r.hash = s.slice(h); s = s.slice(0, h); }
    const scheme = /^([A-Za-z][A-Za-z0-9+.-]*):/.exec(s);
    if (scheme) {
      r.protocol = scheme[1].toLowerCase() + ':';
      s = s.slice(scheme[0].length);
    } else {
      if (!base) return null;
      r.protocol = base.protocol;
      if (base.opaque) {
        if (s !== '') return null;
        return Object.assign({}, base, { hash: r.hash });
      }
      if (!s.startsWith('//') && !(r.protocol in SPECIAL && /^[\\/]{2}/.test(s))) {
        Object.assign(r, { username: base.username, password: base.password, hostname: base.hostname, port: base.port });
        const q = s.indexOf('?');
        let path = q >= 0 ? s.slice(0, q) : s;
        r.search = q >= 0 ? s.slice(q) : '';
        if (path === '') { path = base.pathname; if (q < 0) r.search = base.search; }
        else if (!/^[\\/]/.test(path)) path = base.pathname.replace(/[^/]*$/, '') + path;
        r.pathname = path;
        return finish(r);
      }
    }
    const special = r.protocol in SPECIAL;
    if (special) s = s.replace(/\\/g, '/');
    if (s.startsWith('//')) {
      s = s.slice(2);
      const end = s.search(special ? /[/?]/ : /[/?]/);
      let auth = end >= 0 ? s.slice(0, end) : s;
      s = end >= 0 ? s.slice(end) : '';
      const at = auth.lastIndexOf('@');
      if (at >= 0) {
        const info = auth.slice(0, at);
        auth = auth.slice(at + 1);
        const c = info.indexOf(':');
        r.username = enc(USER_SET)(c >= 0 ? info.slice(0, c) : info);
        r.password = c >= 0 ? enc(USER_SET)(info.slice(c + 1)) : '';
      }
      const port = /:(\d*)$/.exec(auth);
      if (port && !auth.endsWith(']') ) {
        auth = auth.slice(0, port.index);
        if (port[1] !== '') {
          const n = Number(port[1]);
          if (n > 65535) return null;
          r.port = String(n);
        }
      }
      if (special) {
        try { auth = decodeURIComponent(auth); } catch { return null; }
        auth = auth.toLowerCase();
        if (/[\x00-\x20#%/:<>?@[\\\]^|]/.test(auth.replace(/^\[.*\]$/, '')) || (auth === '' && r.protocol !== 'file:')) return null;
      }
      r.hostname = auth;
      if (SPECIAL[r.protocol] === r.port) r.port = '';
    } else if (!special) {
      r.opaque = !s.startsWith('/');
    }
    const q = s.indexOf('?');
    r.pathname = q >= 0 ? s.slice(0, q) : s;
    r.search = q >= 0 ? s.slice(q) : '';
    return finish(r);
  }

  function finish(r) {
    if (r.opaque) r.pathname = r.pathname.replace(/[\x00-\x1f\x7f-￿]/g, encodeURIComponent);
    else {
      if (r.protocol in SPECIAL && r.pathname === '') r.pathname = '/';
      r.pathname = removeDots(enc(PATH_SET)(r.pathname.replace(/\\/g, r.protocol in SPECIAL ? '/' : '\\')));
    }
    if (r.search === '?') r.search = '';
    r.search = enc(QUERY_SET)(r.search);
    if (r.hash === '#') r.hash = '';
    r.hash = enc(FRAGMENT_SET)(r.hash);
    return r;
  }

  const records = new WeakMap();

  class URL {
    #r;
    #params;
    constructor(url, base) {
      let b = null;
      if (base !== undefined) {
        b = parse(base instanceof URL ? base.href : base, null);
        if (!b) throw new TypeError('Invalid base URL: ' + base);
      }
      const r = parse(url instanceof URL ? url.href : url, b);
      if (!r) throw new TypeError('Invalid URL: ' + url);
      this.#r = r;
      this.#params = new URLSearchParams(r.search);
      records.set(this.#params, this);
    }
    static canParse(url, base) { try { new URL(url, base); return true; } catch { return false; } }
    static parse(url, base) { try { return new URL(url, base); } catch { return null; } }
    get protocol() { return this.#r.protocol; }
    set protocol(v) { const p = String(v).replace(/:.*$/, '').toLowerCase() + ':'; if (/^[a-z][a-z0-9+.-]*:$/.test(p) && (p in SPECIAL) === (this.#r.protocol in SPECIAL)) { this.#r.protocol = p; if (SPECIAL[p] === this.#r.port) this.#r.port = ''; } }
    get username() { return this.#r.username; }
    set username(v) { this.#r.username = enc(USER_SET)(String(v)); }
    get password() { return this.#r.password; }
    set password(v) { this.#r.password = enc(USER_SET)(String(v)); }
    get host() { return this.#r.hostname + (this.#r.port ? ':' + this.#r.port : ''); }
    set host(v) { const m = /^([^:/?#]*)(?::(\d*))?/.exec(String(v)); if (m && m[1]) { this.#r.hostname = m[1].toLowerCase(); if (m[2] !== undefined) this.port = m[2]; } }
    get hostname() { return this.#r.hostname; }
    set hostname(v) { const h = String(v).toLowerCase(); if (h && !/[\s/?#:@]/.test(h)) this.#r.hostname = h; }
    get port() { return this.#r.port; }
    set port(v) { const s = String(v); if (s === '') { this.#r.port = ''; return; } const m = /^\d+/.exec(s); if (m && +m[0] <= 65535) this.#r.port = SPECIAL[this.#r.protocol] === String(+m[0]) ? '' : String(+m[0]); }
    get pathname() { return this.#r.pathname; }
    set pathname(v) { if (this.#r.opaque) return; let p = String(v); if (this.#r.protocol in SPECIAL && !p.startsWith('/')) p = '/' + p; this.#r.pathname = removeDots(enc(PATH_SET)(p)); }
    get search() { return this.#r.search; }
    set search(v) { let s = String(v); if (s.startsWith('?')) s = s.slice(1); this.#r.search = s ? '?' + enc(QUERY_SET)(s) : ''; URLSearchParams.reset(this.#params, this.#r.search); }
    get hash() { return this.#r.hash; }
    set hash(v) { let s = String(v); if (s.startsWith('#')) s = s.slice(1); this.#r.hash = s ? '#' + enc(FRAGMENT_SET)(s) : ''; }
    get searchParams() { return this.#params; }
    get origin() { return this.#r.protocol in SPECIAL && this.#r.protocol !== 'file:' ? this.#r.protocol + '//' + this.host : 'null'; }
    get href() {
      const r = this.#r;
      const auth = r.username || r.password ? r.username + (r.password ? ':' + r.password : '') + '@' : '';
      const slashes = r.hostname || r.protocol === 'file:' || (r.protocol in SPECIAL) ? '//' : '';
      return r.protocol + slashes + auth + this.host + r.pathname + r.search + r.hash;
    }
    set href(v) { const r = parse(v, null); if (!r) throw new TypeError('Invalid URL: ' + v); this.#r = r; URLSearchParams.reset(this.#params, r.search); }
    toString() { return this.href; }
    toJSON() { return this.href; }
    static update(params, search) { const u = records.get(params); if (u) u.#r.search = search; }
  }

  const formDecode = (s) => { try { return decodeURIComponent(s.replace(/\+/g, ' ')); } catch { return unescape(s.replace(/\+/g, ' ')); } };
  const formEncode = (s) => encodeURIComponent(s).replace(/%20/g, '+').replace(/[!'()~]/g, (c) => '%' + c.charCodeAt(0).toString(16).toUpperCase());

  class URLSearchParams {
    #pairs = [];
    constructor(init = '') {
      if (init instanceof URLSearchParams) this.#pairs = init.#pairs.map((p) => p.slice());
      else if (init !== null && typeof init === 'object') {
        if (typeof init[Symbol.iterator] === 'function') {
          for (const pair of init) {
            const p = [...pair];
            if (p.length !== 2) throw new TypeError('Each query pair must be an iterable [name, value] tuple');
            this.#pairs.push([String(p[0]), String(p[1])]);
          }
        } else for (const k of Object.keys(init)) this.#pairs.push([k, String(init[k])]);
      } else URLSearchParams.reset(this, String(init));
    }
    static reset(params, search) {
      const s = search.startsWith('?') ? search.slice(1) : search;
      params.#pairs = s.split('&').filter(Boolean).map((part) => {
        const i = part.indexOf('=');
        return i < 0 ? [formDecode(part), ''] : [formDecode(part.slice(0, i)), formDecode(part.slice(i + 1))];
      });
    }
    #changed() { const s = this.toString(); URL.update(this, s ? '?' + s : ''); }
    get size() { return this.#pairs.length; }
    append(k, v) { this.#pairs.push([String(k), String(v)]); this.#changed(); }
    delete(k, v) { k = String(k); this.#pairs = this.#pairs.filter((p) => p[0] !== k || (v !== undefined && p[1] !== String(v))); this.#changed(); }
    get(k) { const e = this.#pairs.find((p) => p[0] === String(k)); return e ? e[1] : null; }
    getAll(k) { return this.#pairs.filter((p) => p[0] === String(k)).map((p) => p[1]); }
    has(k, v) { return this.#pairs.some((p) => p[0] === String(k) && (v === undefined || p[1] === String(v))); }
    set(k, v) {
      k = String(k);
      const i = this.#pairs.findIndex((p) => p[0] === k);
      if (i < 0) this.#pairs.push([k, String(v)]);
      else { this.#pairs[i][1] = String(v); this.#pairs = this.#pairs.filter((p, j) => j <= i || p[0] !== k); }
      this.#changed();
    }
    sort() { this.#pairs.sort((a, b) => (a[0] < b[0] ? -1 : a[0] > b[0] ? 1 : 0)); this.#changed(); }
    forEach(f, self) { for (const [k, v] of this.#pairs) f.call(self, v, k, this); }
    keys() { return this.#pairs.map((p) => p[0])[Symbol.iterator](); }
    values() { return this.#pairs.map((p) => p[1])[Symbol.iterator](); }
    entries() { return this.#pairs.map((p) => p.slice())[Symbol.iterator](); }
    [Symbol.iterator]() { return this.entries(); }
    toString() { return this.#pairs.map(([k, v]) => formEncode(k) + '=' + formEncode(v)).join('&'); }
    get [Symbol.toStringTag]() { return 'URLSearchParams'; }
  }

  for (const c of [URL, URLSearchParams]) Object.defineProperty(globalThis, c.name, { value: c, writable: true, configurable: true });
})();
