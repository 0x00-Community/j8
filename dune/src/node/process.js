// process (an EventEmitter over the Rust `process` object) and the Node globals: global, Buffer, require,
// setImmediate, Timeout-returning timers.
(function (native) {
  'use strict';
  const { stdio, clock, os } = native;
  const Buffer = { from: (...args) => native.builtin('buffer').Buffer.from(...args), wrap: (b) => native.builtin('buffer').Buffer.from(b.buffer, b.byteOffset, b.byteLength) };
  const process = globalThis.process;
  const nodeVersion = '25.9.0';
  Object.assign(process, {
    platform: os.platform(),
    arch: os.arch(),
    env: os.env(),
    version: 'v' + nodeVersion,
    versions: { node: nodeVersion, dune: os.version() },
  });
  const EventEmitter = native.builtin('events');
  Object.setPrototypeOf(process, EventEmitter.prototype);
  EventEmitter.init.call(process);

  function stream(fd, write) {
    const s = new EventEmitter();
    Object.assign(s, {
      fd,
      isTTY: stdio.isTty(fd),
      columns: 80,
      rows: 24,
      writable: true,
      write(chunk, encoding, cb) {
        if (typeof encoding === 'function') cb = encoding;
        write(typeof chunk === 'string' ? chunk : Buffer.from(chunk).toString(typeof encoding === 'string' ? encoding : 'utf8'));
        if (typeof cb === 'function') queueMicrotask(cb);
        return true;
      },
      end(chunk, cb) { if (chunk !== undefined && typeof chunk !== 'function') s.write(chunk); if (typeof (cb || chunk) === 'function') queueMicrotask(cb || chunk); },
      getColorDepth: () => (s.isTTY ? 8 : 1),
      hasColors: () => s.isTTY,
      cursorTo() { return true; },
      clearLine() { return true; },
    });
    return s;
  }

  let exiting = false;
  function finish(code) {
    if (code !== undefined) process.exitCode = code;
    if (!exiting) {
      exiting = true;
      process.emit('exit', process.exitCode | 0);
    }
    return process.exitCode | 0;
  }

  Object.assign(process, {
    title: 'dune',
    release: { name: 'node' },
    argv0: 'dune',
    execArgv: [],
    exitCode: undefined,
    config: { variables: {} },
    features: { inspector: false, ipv6: true, tls: false },
    stdout: stream(1, stdio.out),
    stderr: stream(2, stdio.err),
    stdin: Object.assign(new EventEmitter(), { fd: 0, isTTY: stdio.isTty(0) }),
    exit(code) { stdio.exit(finish(code)); },
    abort() { stdio.exit(134); },
    nextTick(f, ...args) {
      if (typeof f !== 'function') throw new TypeError('The "callback" argument must be of type function');
      Promise.resolve().then(() => { try { f(...args); } catch (e) { native.reportError(e); } });
    },
    hrtime(prev) {
      const ns = Math.round(clock.now() * 1e6);
      let s = Math.floor(ns / 1e9);
      let n = ns % 1e9;
      if (prev) { s -= prev[0]; n -= prev[1]; if (n < 0) { s--; n += 1e9; } }
      return [s, n];
    },
    memoryUsage() { return { rss: process.rss(), heapTotal: 0, heapUsed: 0, external: 0, arrayBuffers: 0 }; },
    cpuUsage: () => ({ user: Math.round(clock.now() * 1e3), system: 0 }),
    resourceUsage: () => ({}),
    emitWarning(warning, type, code) {
      if (typeof type === 'object' && type) ({ type, code } = type);
      const w = warning instanceof Error ? warning : Object.assign(new Error(String(warning)), { name: type || 'Warning' });
      if (code) w.code = code;
      if (!process.emit('warning', w)) process.stderr.write('(dune:' + process.pid + ') ' + (code ? '[' + code + '] ' : '') + w.name + ': ' + w.message + '\n');
    },
    umask: () => 0o22,
    getuid: () => -1,
    getgid: () => -1,
    kill() { throw new Error('process.kill is not supported'); },
    binding() { throw new Error('No such module'); },
  });
  process.hrtime.bigint = () => BigInt(Math.round(clock.now() * 1e6));
  process.memoryUsage.rss = () => process.rss();

  // An uncaught error goes to 'uncaughtException' listeners first; at the end of the run, 'exit'
  native.onUncaught = (e) => {
    if (process.listenerCount('uncaughtException') === 0) return false;
    process.emit('uncaughtException', e, 'uncaughtException');
    return true;
  };
  native.finish = () => finish();
  native.define('process', () => process);

  const define = (name, value) => Object.defineProperty(globalThis, name, { value, writable: true, configurable: true });
  define('global', globalThis);
  const lazy = (name, get) => Object.defineProperty(globalThis, name, {
    configurable: true,
    get() { const value = get(); define(name, value); return value; },
    set(value) { define(name, value); },
  });
  lazy('Buffer', () => native.builtin('buffer').Buffer);
  for (const [name, f] of Object.entries(native.nodeTimers)) define(name, f);
  // `require` for -e / -p / the REPL: relative to the working directory
  define('require', native.makeRequire(new native.Module(process.cwd() + '/[eval]')));
})(globalThis.__j8_native);
