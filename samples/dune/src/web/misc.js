// structuredClone, performance, crypto, navigator, self.
(function (native) {
  'use strict';
  const { clock, random } = native;

  function structuredClone(value, options) {
    const seen = new Map();
    const transfer = options && options.transfer ? options.transfer : [];
    function clone(v) {
      if (v === null || (typeof v !== 'object' && typeof v !== 'function')) {
        if (typeof v === 'symbol') throw new DOMException(String(v) + ' could not be cloned.', 'DataCloneError');
        return v;
      }
      if (typeof v === 'function') throw new DOMException((v.name || 'function') + ' could not be cloned.', 'DataCloneError');
      if (seen.has(v)) return seen.get(v);
      let out;
      if (v instanceof Date) out = new Date(v.getTime());
      else if (v instanceof RegExp) out = new RegExp(v.source, v.flags);
      else if (v instanceof ArrayBuffer) out = transfer.includes(v) ? v : v.slice(0);
      else if (ArrayBuffer.isView(v)) {
        const buffer = clone(v.buffer);
        out = v instanceof DataView ? new DataView(buffer, v.byteOffset, v.byteLength) : new v.constructor(buffer, v.byteOffset, v.length);
      } else if (v instanceof Map) { out = new Map(); seen.set(v, out); for (const [k, x] of v) out.set(clone(k), clone(x)); return out; }
      else if (v instanceof Set) { out = new Set(); seen.set(v, out); for (const x of v) out.add(clone(x)); return out; }
      else if (v instanceof Error) {
        const Ctor = { EvalError, RangeError, ReferenceError, SyntaxError, TypeError, URIError }[v.name] || Error;
        out = new Ctor(v.message);
        if ('cause' in v) out.cause = clone(v.cause);
      } else if (v instanceof Boolean || v instanceof Number || v instanceof String) out = Object(v.valueOf());
      else if (v instanceof Blob) out = v.slice(0, v.size, v.type);
      else if (Array.isArray(v)) { out = new Array(v.length); seen.set(v, out); for (const k of Object.keys(v)) out[k] = clone(v[k]); return out; }
      else if (v instanceof WeakMap || v instanceof WeakSet || v instanceof Promise || v instanceof WeakRef) throw new DOMException(Object.prototype.toString.call(v) + ' could not be cloned.', 'DataCloneError');
      else { out = {}; seen.set(v, out); for (const k of Object.keys(v)) out[k] = clone(v[k]); return out; }
      seen.set(v, out);
      return out;
    }
    return clone(value);
  }

  const marks = [];
  class PerformanceEntry {
    constructor(name, entryType, startTime, duration, detail) { Object.assign(this, { name, entryType, startTime, duration, detail: detail === undefined ? null : detail }); }
    toJSON() { return { ...this }; }
  }
  const performance = {
    get timeOrigin() { return clock.wall() - clock.now(); },
    now: () => clock.now(),
    mark(name, options = {}) {
      const e = new PerformanceEntry(String(name), 'mark', options.startTime === undefined ? clock.now() : options.startTime, 0, options.detail);
      marks.push(e);
      return e;
    },
    measure(name, start, end) {
      const at = (m, fallback) => (m === undefined ? fallback : typeof m === 'number' ? m : (marks.findLast((e) => e.name === m) || { startTime: NaN }).startTime);
      const s = typeof start === 'object' && start ? at(start.start, 0) : at(start, 0);
      const t = typeof start === 'object' && start ? at(start.end, clock.now()) : at(end, clock.now());
      const e = new PerformanceEntry(String(name), 'measure', s, t - s);
      marks.push(e);
      return e;
    },
    getEntries: () => marks.slice(),
    getEntriesByName: (n, type) => marks.filter((e) => e.name === n && (!type || e.entryType === type)),
    getEntriesByType: (type) => marks.filter((e) => e.entryType === type),
    clearMarks: (n) => { for (let i = marks.length - 1; i >= 0; i--) if (marks[i].entryType === 'mark' && (n === undefined || marks[i].name === n)) marks.splice(i, 1); },
    clearMeasures: (n) => { for (let i = marks.length - 1; i >= 0; i--) if (marks[i].entryType === 'measure' && (n === undefined || marks[i].name === n)) marks.splice(i, 1); },
    toJSON() { return { timeOrigin: this.timeOrigin }; },
  };

  const INTEGER_ARRAYS = [Int8Array, Uint8Array, Uint8ClampedArray, Int16Array, Uint16Array, Int32Array, Uint32Array, BigInt64Array, BigUint64Array];
  const crypto = {
    getRandomValues(array) {
      if (!INTEGER_ARRAYS.some((T) => array instanceof T)) throw new DOMException('The data argument must be an integer-type TypedArray', 'TypeMismatchError');
      if (array.byteLength > 65536) throw new DOMException('The ArrayBufferView byte length exceeds 65536', 'QuotaExceededError');
      new Uint8Array(array.buffer, array.byteOffset, array.byteLength).set(random.bytes(array.byteLength));
      return array;
    },
    randomUUID() {
      const b = random.bytes(16);
      b[6] = (b[6] & 0x0f) | 0x40;
      b[8] = (b[8] & 0x3f) | 0x80;
      const h = Array.from(b, (x) => x.toString(16).padStart(2, '0')).join('');
      return h.slice(0, 8) + '-' + h.slice(8, 12) + '-' + h.slice(12, 16) + '-' + h.slice(16, 20) + '-' + h.slice(20);
    },
  };

  const navigator = { userAgent: 'dune', language: 'en-US', languages: ['en-US'], hardwareConcurrency: 1 };

  for (const [n, v] of Object.entries({ structuredClone, performance, crypto, navigator, PerformanceEntry })) {
    Object.defineProperty(globalThis, n, { value: v, writable: true, configurable: true });
  }
  Object.defineProperty(globalThis, 'self', { get: () => globalThis, configurable: true });
})(globalThis.__j8_native);

// Entry points the host calls: run a file as an ES module; the exit code once the event loop is idle
(function (native) {
  'use strict';
  native.runModule = (file) => { import(file).catch((e) => native.reportError(e)); };
  native.finish = () => 0;
})(globalThis.__j8_native);
