// node:util — format / inspect (shared with console), promisify, callbackify, inherits, deprecate, types,
// isDeepStrictEqual, parseArgs, styleText, debuglog.
(function (native) {
  'use strict';
  const { inspect, format } = native;
  const kCustomPromisify = Symbol.for('nodejs.util.promisify.custom');
  const tag = (v) => Object.prototype.toString.call(v).slice(8, -1);
  const typedArrayProto = Object.getPrototypeOf(Uint8Array.prototype);

  function promisify(f) {
    if (typeof f !== 'function') throw new TypeError('The "original" argument must be of type function');
    if (f[kCustomPromisify]) return f[kCustomPromisify];
    const p = function (...args) {
      return new Promise((resolve, reject) => {
        f.call(this, ...args, (err, ...values) => (err ? reject(err) : resolve(values.length > 1 ? values : values[0])));
      });
    };
    Object.setPrototypeOf(p, Object.getPrototypeOf(f));
    Object.defineProperty(p, kCustomPromisify, { value: p });
    return Object.defineProperties(p, Object.getOwnPropertyDescriptors(f));
  }
  promisify.custom = kCustomPromisify;

  function callbackify(f) {
    return function (...args) {
      const cb = args.pop();
      if (typeof cb !== 'function') throw new TypeError('The last argument must be of type function');
      f.apply(this, args).then((v) => queueMicrotask(() => cb(null, v)), (e) => queueMicrotask(() => cb(e || Object.assign(new Error('Promise was rejected with a falsy value'), { reason: e }))));
    };
  }

  /** Deep equality with Node's strict rules (prototypes, own enumerable keys, Map / Set contents) */
  function deepEqual(a, b, strict, memo = new Map()) {
    if (strict ? Object.is(a, b) : a == b || (a !== a && b !== b)) return true;
    if (typeof a !== 'object' || typeof b !== 'object' || a === null || b === null) return false;
    if (strict && Object.getPrototypeOf(a) !== Object.getPrototypeOf(b)) return false;
    if (tag(a) !== tag(b)) return false;
    if (memo.get(a) === b) return true;
    memo.set(a, b);
    if (a instanceof Date) return a.getTime() === b.getTime();
    if (a instanceof RegExp) return a.source === b.source && a.flags === b.flags && a.lastIndex === b.lastIndex;
    if (a instanceof Error && (a.message !== b.message || a.name !== b.name)) return false;
    if (ArrayBuffer.isView(a)) {
      if (a.byteLength !== b.byteLength) return false;
      const x = new Uint8Array(a.buffer, a.byteOffset, a.byteLength);
      const y = new Uint8Array(b.buffer, b.byteOffset, b.byteLength);
      for (let i = 0; i < x.length; i++) if (x[i] !== y[i]) return false;
    }
    if (a instanceof Map) {
      if (a.size !== b.size) return false;
      for (const [k, v] of a) if (!b.has(k) || !deepEqual(v, b.get(k), strict, memo)) return false;
    }
    if (a instanceof Set) {
      if (a.size !== b.size) return false;
      outer: for (const v of a) {
        if (b.has(v)) continue;
        if (typeof v === 'object') for (const w of b) if (deepEqual(v, w, strict, memo)) continue outer;
        return false;
      }
    }
    for (const Box of [Number, String, Boolean]) if (a instanceof Box && !Object.is(a.valueOf(), b.valueOf())) return false;
    const ka = Object.keys(a);
    const kb = Object.keys(b);
    if (ka.length !== kb.length) return false;
    for (const k of ka) if (!Object.prototype.hasOwnProperty.call(b, k) || !deepEqual(a[k], b[k], strict, memo)) return false;
    if (strict) {
      const sa = Object.getOwnPropertySymbols(a).filter((s) => Object.prototype.propertyIsEnumerable.call(a, s));
      const sb = Object.getOwnPropertySymbols(b).filter((s) => Object.prototype.propertyIsEnumerable.call(b, s));
      if (sa.length !== sb.length || sa.some((s) => !deepEqual(a[s], b[s], strict, memo))) return false;
    }
    return true;
  }
  native.deepEqual = deepEqual;

  const types = {
    isPromise: (v) => v instanceof Promise,
    isDate: (v) => v instanceof Date,
    isRegExp: (v) => v instanceof RegExp,
    isMap: (v) => v instanceof Map,
    isSet: (v) => v instanceof Set,
    isWeakMap: (v) => v instanceof WeakMap,
    isWeakSet: (v) => v instanceof WeakSet,
    isNativeError: (v) => v instanceof Error,
    isArrayBuffer: (v) => v instanceof ArrayBuffer,
    isSharedArrayBuffer: (v) => typeof SharedArrayBuffer === 'function' && v instanceof SharedArrayBuffer,
    isAnyArrayBuffer: (v) => v instanceof ArrayBuffer || (typeof SharedArrayBuffer === 'function' && v instanceof SharedArrayBuffer),
    isArrayBufferView: (v) => ArrayBuffer.isView(v),
    isTypedArray: (v) => v instanceof typedArrayProto.constructor,
    isUint8Array: (v) => v instanceof Uint8Array,
    isDataView: (v) => v instanceof DataView,
    isAsyncFunction: (v) => typeof v === 'function' && tag(v) === 'AsyncFunction',
    isGeneratorFunction: (v) => typeof v === 'function' && /^(GeneratorFunction|AsyncGeneratorFunction)$/.test(tag(v)),
    isGeneratorObject: (v) => tag(v) === 'Generator',
    isBoxedPrimitive: (v) => v instanceof Number || v instanceof String || v instanceof Boolean || (typeof v === 'object' && v !== null && (tag(v) === 'Symbol' || tag(v) === 'BigInt')),
    isProxy: () => false,
    isMapIterator: (v) => tag(v) === 'Map Iterator',
    isSetIterator: (v) => tag(v) === 'Set Iterator',
  };

  const STYLES = { reset: [0, 0], bold: [1, 22], dim: [2, 22], italic: [3, 23], underline: [4, 24], inverse: [7, 27], hidden: [8, 28], strikethrough: [9, 29], black: [30, 39], red: [31, 39], green: [32, 39], yellow: [33, 39], blue: [34, 39], magenta: [35, 39], cyan: [36, 39], white: [37, 39], gray: [90, 39], grey: [90, 39], bgRed: [41, 49], bgGreen: [42, 49], bgYellow: [43, 49], bgBlue: [44, 49] };
  function styleText(format, text) {
    for (const f of [].concat(format)) {
      const s = STYLES[f];
      if (!s) throw new TypeError('The argument "format" must be one of ' + Object.keys(STYLES).join(', '));
      text = '\x1b[' + s[0] + 'm' + text + '\x1b[' + s[1] + 'm';
    }
    return text;
  }

  /** util.parseArgs: --long, --long=value, -s, -abc, positionals after `--` or with allowPositionals */
  function parseArgs(config = {}) {
    const args = config.args || process.argv.slice(2);
    const options = config.options || {};
    const strict = config.strict !== false;
    const values = Object.create(null);
    const positionals = [];
    const byShort = {};
    for (const [k, o] of Object.entries(options)) if (o.short) byShort[o.short] = k;
    const err = (m) => { const e = new TypeError(m); e.code = 'ERR_PARSE_ARGS_UNKNOWN_OPTION'; throw e; };
    const store = (name, value) => {
      const o = options[name];
      if (!o) { if (strict) err("Unknown option '" + name + "'"); values[name] = value === undefined ? true : value; return; }
      if (o.type === 'string' && value === undefined) { const e = new TypeError("Option '--" + name + " <value>' argument missing"); e.code = 'ERR_PARSE_ARGS_INVALID_OPTION_VALUE'; throw e; }
      const v = o.type === 'boolean' ? true : value;
      if (o.multiple) (values[name] = values[name] || []).push(v); else values[name] = v;
    };
    for (let i = 0; i < args.length; i++) {
      const a = args[i];
      if (a === '--') { positionals.push(...args.slice(i + 1)); break; }
      if (a.startsWith('--')) {
        const eq = a.indexOf('=');
        const name = eq >= 0 ? a.slice(2, eq) : a.slice(2);
        const wants = options[name] && options[name].type === 'string';
        store(name, eq >= 0 ? a.slice(eq + 1) : wants ? args[++i] : undefined);
      } else if (a.startsWith('-') && a.length > 1) {
        for (let j = 1; j < a.length; j++) {
          const name = byShort[a[j]] || a[j];
          if (options[name] && options[name].type === 'string') { store(name, j + 1 < a.length ? a.slice(j + 1) : args[++i]); break; }
          store(name, undefined);
        }
      } else {
        if (strict && !config.allowPositionals) { const e = new TypeError("Unexpected argument '" + a + "'. This command does not take positional arguments"); e.code = 'ERR_PARSE_ARGS_UNEXPECTED_POSITIONAL'; throw e; }
        positionals.push(a);
      }
    }
    for (const [k, o] of Object.entries(options)) if (o.default !== undefined && !(k in values)) values[k] = o.default;
    return { values, positionals };
  }

  const warned = new Set();
  native.define('util', () => ({
    format,
    formatWithOptions: (opts, ...args) => format(...args),
    inspect,
    promisify,
    callbackify,
    inherits(ctor, superCtor) {
      Object.defineProperty(ctor, 'super_', { value: superCtor, writable: true, configurable: true });
      Object.setPrototypeOf(ctor.prototype, superCtor.prototype);
    },
    deprecate(f, message, code) {
      return function (...args) {
        if (!warned.has(code || message)) { warned.add(code || message); process.emitWarning(message, 'DeprecationWarning', code); }
        return new.target ? Reflect.construct(f, args, new.target) : f.apply(this, args);
      };
    },
    isDeepStrictEqual: (a, b) => deepEqual(a, b, true),
    types,
    isArray: Array.isArray,
    TextEncoder,
    TextDecoder,
    debuglog: (section) => {
      const on = new RegExp('\\b' + String(section) + '\\b', 'i').test(process.env.NODE_DEBUG || '');
      const log = (...args) => { if (on) process.stderr.write(String(section).toUpperCase() + ' ' + process.pid + ': ' + format(...args) + '\n'); };
      log.enabled = on;
      return log;
    },
    stripVTControlCharacters: (s) => String(s).replace(/\x1b\[[0-9;]*[A-Za-z]/g, ''),
    styleText,
    parseArgs,
    toUSVString: (s) => String(s).toWellFormed(),
    aborted: (signal) => new Promise((resolve) => signal.addEventListener('abort', resolve, { once: true })),
  }));
  native.define('util/types', () => types);
})(globalThis.__j8_native);
