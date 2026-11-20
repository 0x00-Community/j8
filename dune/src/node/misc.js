// The smaller built-ins: os, url, timers (+ timers/promises), child_process, crypto, string_decoder, perf_hooks.
(function (native) {
  'use strict';
  const { Buffer, timers, random, child, codec } = native;
  const os = native.os;

  native.define('os', () => {
    const cores = os.cores();
    const cpu = { model: 'unknown', speed: 0, times: { user: 0, nice: 0, sys: 0, idle: 0, irq: 0 } };
    return {
      EOL: '\n',
      devNull: '/dev/null',
      constants: { signals: { SIGHUP: 1, SIGINT: 2, SIGQUIT: 3, SIGKILL: 9, SIGTERM: 15 }, errno: {} },
      platform: () => process.platform,
      type: () => (process.platform === 'linux' ? 'Linux' : process.platform === 'darwin' ? 'Darwin' : process.platform),
      arch: () => process.arch,
      machine: () => ({ x64: 'x86_64', arm64: 'aarch64' }[process.arch] || process.arch),
      release: () => os.release(),
      version: () => os.release(),
      hostname: () => os.hostname(),
      homedir: () => os.homedir(),
      tmpdir: () => os.tmpdir(),
      cpus: () => Array.from({ length: cores }, () => structuredClone(cpu)),
      availableParallelism: () => cores,
      totalmem: () => os.memory()[0],
      freemem: () => os.memory()[1],
      loadavg: () => os.loadavg(),
      uptime: () => os.uptime(),
      endianness: () => 'LE',
      userInfo: () => ({ username: os.username(), homedir: os.homedir(), shell: process.env.SHELL || null, uid: -1, gid: -1 }),
      networkInterfaces: () => ({}),
    };
  });

  native.define('url', () => ({
    URL,
    URLSearchParams,
    fileURLToPath(u) {
      const url = typeof u === 'string' ? new URL(u) : u;
      if (url.protocol !== 'file:') { const e = new TypeError('The URL must be of scheme file'); e.code = 'ERR_INVALID_URL_SCHEME'; throw e; }
      return decodeURIComponent(url.pathname);
    },
    pathToFileURL: (p) => new URL('file://' + encodeURI(native.builtin('path').resolve(p)).replace(/[?#]/g, encodeURIComponent)),
    format: (u) => (u instanceof URL ? u.href : String(u.href || '')),
    parse: (s) => { const u = new URL(s); return { protocol: u.protocol, host: u.host, hostname: u.hostname, port: u.port || null, pathname: u.pathname, search: u.search || null, query: u.search.slice(1) || null, hash: u.hash || null, href: u.href, path: u.pathname + u.search }; },
  }));

  // Timeout objects over the web layer's timers: ref / unref / refresh, and a number when coerced
  class Timeout {
    #id;
    constructor(id) { this.#id = id; }
    ref() { timers.ref(this.#id, true); return this; }
    unref() { timers.ref(this.#id, false); return this; }
    hasRef() { return timers.has(this.#id); }
    refresh() { timers.refresh(this.#id); return this; }
    close() { timers.clear(this.#id); return this; }
    [Symbol.toPrimitive]() { return this.#id; }
    [Symbol.dispose]() { this.close(); }
    static id(t) { return t instanceof Timeout ? t.#id : +t; }
  }
  class Immediate extends Timeout {}
  const api = {
    setTimeout: (f, ms, ...args) => new Timeout(timers.add(f, ms, args, false)),
    setInterval: (f, ms, ...args) => new Timeout(timers.add(f, ms, args, true)),
    setImmediate: (f, ...args) => new Immediate(timers.add(f, 0, args, false)),
    clearTimeout: (t) => { if (t != null) timers.clear(Timeout.id(t)); },
    clearInterval: (t) => { if (t != null) timers.clear(Timeout.id(t)); },
    clearImmediate: (t) => { if (t != null) timers.clear(Timeout.id(t)); },
  };
  native.nodeTimers = api;
  native.define('timers', () => api);
  native.define('timers/promises', () => {
    const later = (ms, value, options = {}) => new Promise((resolve, reject) => {
      if (options.signal && options.signal.aborted) return reject(options.signal.reason);
      const t = api.setTimeout(() => resolve(value), ms);
      if (options.ref === false) t.unref();
      if (options.signal) options.signal.addEventListener('abort', () => { t.close(); reject(options.signal.reason); }, { once: true });
    });
    return {
      setTimeout: later,
      setImmediate: (value, options) => later(0, value, options),
      async *setInterval(ms, value) { for (;;) yield await later(ms, value); },
      scheduler: { wait: (ms, options) => later(ms, undefined, options), yield: () => later(0) },
    };
  });

  /** A failed child: Node's error shape (status, signal, stdout, stderr) */
  function childError(cmd, r) {
    const e = new Error('Command failed: ' + cmd + (r.stderr && r.stderr.length ? '\n' + r.stderr.toString() : ''));
    return Object.assign(e, { status: r.status, signal: r.signal, stdout: r.stdout, stderr: r.stderr, output: [null, r.stdout, r.stderr], pid: 0 });
  }
  /** `stdio` as three of 'pipe' / 'inherit' / 'ignore': a stream object or its fd is 'inherit', null is 'pipe' */
  function streams(stdio) {
    const one = (s, fd) => {
      if (s === 'inherit' || s === 'ignore') return s;
      if (s === fd || (fd === 0 && s === process.stdin) || (fd === 1 && s === process.stdout) || (fd === 2 && s === process.stderr)) return 'inherit';
      return 'pipe';
    };
    const all = typeof stdio === 'string' ? [stdio, stdio, stdio] : Array.isArray(stdio) ? stdio : [];
    return [0, 1, 2].map((fd) => one(all[fd], fd));
  }
  function spawnSync(file, args = [], o = {}) {
    if (!Array.isArray(args)) { o = args || {}; args = []; }
    const input = o.input === undefined ? null : typeof o.input === 'string' ? Buffer.from(o.input) : o.input;
    const env = o.env ? Object.fromEntries(Object.entries(o.env).filter(([, v]) => v !== undefined).map(([k, v]) => [k, String(v)])) : null;
    const options = { shell: !!o.shell, cwd: o.cwd === undefined ? null : String(o.cwd), env, stdio: streams(o.stdio) };
    let r;
    try {
      const [status, stdout, stderr] = child.spawnSync(String(file), args.map(String), options, input);
      const out = (b) => (o.encoding && o.encoding !== 'buffer' ? Buffer.wrap(b).toString(o.encoding) : Buffer.wrap(b));
      r = { pid: 0, status, signal: status === null ? 'SIGTERM' : null, stdout: out(stdout), stderr: out(stderr) };
    } catch (e) {
      r = { pid: 0, status: null, signal: null, stdout: null, stderr: null, error: e };
    }
    r.output = [null, r.stdout, r.stderr];
    return r;
  }
  function execFileSync(file, args, o) {
    if (!Array.isArray(args)) { o = args; args = []; }
    const r = spawnSync(file, args, o);
    if (r.error) throw r.error;
    if (r.status !== 0) throw childError([file, ...args].join(' '), r);
    if (!(o && o.stdio === 'pipe') && r.stderr && r.stderr.length) process.stderr.write(r.stderr);
    return r.stdout;
  }
  const execSync = (cmd, o = {}) => execFileSync(cmd, [], Object.assign({}, o, { shell: true }));
  native.define('child_process', () => ({
    spawnSync,
    execSync,
    execFileSync,
    exec(cmd, o, cb) {
      if (typeof o === 'function') { cb = o; o = {}; }
      const enc = (o && o.encoding) || 'utf8';
      Promise.resolve().then(() => {
        const r = spawnSync(cmd, [], Object.assign({}, o, { shell: true, encoding: enc }));
        if (cb) cb(r.error || (r.status !== 0 ? childError(cmd, r) : null), r.stdout, r.stderr);
      });
    },
    execFile(file, args, o, cb) {
      if (typeof args === 'function') { cb = args; args = []; o = {}; } else if (typeof o === 'function') { cb = o; o = {}; }
      Promise.resolve().then(() => {
        const r = spawnSync(file, args, Object.assign({ encoding: 'utf8' }, o));
        if (cb) cb(r.error || (r.status !== 0 ? childError(file, r) : null), r.stdout, r.stderr);
      });
    },
  }));

  native.define('crypto', () => ({
    randomBytes(n, cb) {
      const b = Buffer.wrap(random.bytes(n));
      if (typeof cb === 'function') { queueMicrotask(() => cb(null, b)); return undefined; }
      return b;
    },
    randomUUID: () => crypto.randomUUID(),
    randomInt(min, max, cb) {
      if (max === undefined || typeof max === 'function') { cb = max; max = min; min = 0; }
      const range = max - min;
      if (!(range > 0 && range <= 2 ** 48)) throw new RangeError('The value of "max" is out of range.');
      const b = random.bytes(6);
      const n = min + Math.floor((b.reduce((acc, x) => acc * 256 + x, 0) / 2 ** 48) * range);
      if (typeof cb === 'function') { queueMicrotask(() => cb(null, n)); return undefined; }
      return n;
    },
    getRandomValues: (a) => crypto.getRandomValues(a),
    webcrypto: crypto,
    timingSafeEqual(a, b) {
      if (a.byteLength !== b.byteLength) throw new RangeError('Input buffers must have the same byte length');
      let d = 0;
      for (let i = 0; i < a.length; i++) d |= a[i] ^ b[i];
      return d === 0;
    },
  }));

  /** Decodes UTF-8 (or another encoding) in pieces, holding back an incomplete character */
  class StringDecoder {
    constructor(encoding = 'utf8') { this.encoding = String(encoding).toLowerCase().replace('-', ''); this.pending = new Uint8Array(0); }
    write(chunk) {
      const bytes = new Uint8Array(this.pending.length + chunk.length);
      bytes.set(this.pending);
      bytes.set(chunk, this.pending.length);
      if (this.encoding !== 'utf8') { this.pending = new Uint8Array(0); return Buffer.wrap(bytes).toString(this.encoding); }
      let end = bytes.length;
      for (let i = Math.max(0, bytes.length - 3); i < bytes.length; i++) {
        const b = bytes[i];
        const need = b >= 0xf0 ? 4 : b >= 0xe0 ? 3 : b >= 0xc0 ? 2 : 0;
        if (need && i + need > bytes.length) { end = i; break; }
      }
      this.pending = bytes.slice(end);
      return codec.utf8Decode(bytes.subarray(0, end));
    }
    end(chunk) {
      const s = chunk ? this.write(chunk) : '';
      const rest = this.pending.length ? codec.utf8Decode(this.pending) : '';
      this.pending = new Uint8Array(0);
      return s + rest;
    }
  }
  native.define('string_decoder', () => ({ StringDecoder }));
  native.define('perf_hooks', () => ({ performance, PerformanceEntry }));
})(globalThis.__j8_native);
