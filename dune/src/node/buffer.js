// node:buffer — Buffer (a Uint8Array subclass) with Node's encodings and read / write helpers.
(function (native) {
  'use strict';
  const { codec } = native;
  const ENCODINGS = { utf8: 'utf8', 'utf-8': 'utf8', hex: 'hex', base64: 'base64', base64url: 'base64url', latin1: 'latin1', binary: 'latin1', ascii: 'ascii', utf16le: 'utf16le', 'utf-16le': 'utf16le', ucs2: 'utf16le', 'ucs-2': 'utf16le' };
  const encodingOf = (e) => {
    if (e === undefined || e === null) return 'utf8';
    const n = ENCODINGS[String(e).toLowerCase()];
    if (!n) { const err = new TypeError('Unknown encoding: ' + e); err.code = 'ERR_UNKNOWN_ENCODING'; throw err; }
    return n;
  };

  function encode(string, encoding) {
    const s = String(string);
    switch (encodingOf(encoding)) {
      case 'utf8': return codec.utf8Encode(s);
      case 'hex': {
        const n = s.length >> 1;
        const out = new Uint8Array(n);
        let i = 0;
        for (; i < n; i++) { const b = parseInt(s.substr(i * 2, 2), 16); if (Number.isNaN(b)) break; out[i] = b; }
        return out.subarray(0, i);
      }
      case 'base64':
      case 'base64url': return codec.base64Decode(s.replace(/[^A-Za-z0-9+/\-_]/g, '')) || new Uint8Array(0);
      case 'latin1':
      case 'ascii': { const out = new Uint8Array(s.length); for (let i = 0; i < s.length; i++) out[i] = s.charCodeAt(i); return out; }
      case 'utf16le': { const out = new Uint8Array(s.length * 2); for (let i = 0; i < s.length; i++) { const c = s.charCodeAt(i); out[i * 2] = c & 0xff; out[i * 2 + 1] = c >> 8; } return out; }
    }
  }

  function decode(bytes, encoding) {
    switch (encodingOf(encoding)) {
      case 'utf8': return codec.utf8Decode(bytes);
      case 'hex': { let s = ''; for (const b of bytes) s += (b < 16 ? '0' : '') + b.toString(16); return s; }
      case 'base64': return codec.base64Encode(bytes);
      case 'base64url': return codec.base64Encode(bytes).replace(/\+/g, '-').replace(/\//g, '_').replace(/=+$/, '');
      case 'latin1': { let s = ''; for (let i = 0; i < bytes.length; i += 4096) s += String.fromCharCode.apply(null, bytes.subarray(i, i + 4096)); return s; }
      case 'ascii': { let s = ''; for (const b of bytes) s += String.fromCharCode(b & 0x7f); return s; }
      case 'utf16le': { let s = ''; for (let i = 0; i + 1 < bytes.length; i += 2) s += String.fromCharCode(bytes[i] | (bytes[i + 1] << 8)); return s; }
    }
  }

  const range = (name, v, max) => { if (v < 0 || v > max) { const e = new RangeError('The value of "' + name + '" is out of range.'); e.code = 'ERR_OUT_OF_RANGE'; throw e; } };

  class Buffer extends Uint8Array {
    static from(value, encodingOrOffset, length) {
      if (typeof value === 'string') return Buffer.wrap(encode(value, encodingOrOffset));
      if (value instanceof ArrayBuffer || (typeof SharedArrayBuffer === 'function' && value instanceof SharedArrayBuffer)) {
        const off = encodingOrOffset === undefined ? 0 : +encodingOrOffset;
        return new Buffer(value, off, length === undefined ? value.byteLength - off : +length);
      }
      if (ArrayBuffer.isView(value)) return Buffer.wrap(new Uint8Array(value.buffer, value.byteOffset, value.byteLength).slice());
      if (value && typeof value === 'object') {
        if (value.type === 'Buffer' && Array.isArray(value.data)) return Buffer.wrap(Uint8Array.from(value.data));
        if (typeof value.length === 'number' || Array.isArray(value)) return Buffer.wrap(Uint8Array.from(value));
        if (typeof value.valueOf === 'function' && value.valueOf() !== value) return Buffer.from(value.valueOf(), encodingOrOffset, length);
      }
      throw new TypeError('The first argument must be of type string or an instance of Buffer, ArrayBuffer, or Array or an Array-like Object.');
    }
    /** A Buffer viewing the same memory as `u8` */
    static wrap(u8) { return new Buffer(u8.buffer, u8.byteOffset, u8.byteLength); }
    static alloc(size, fill, encoding) { const b = new Buffer(size); if (fill !== undefined && fill !== 0) b.fill(fill, 0, b.length, encoding); return b; }
    static allocUnsafe(size) { return new Buffer(size); }
    static allocUnsafeSlow(size) { return new Buffer(size); }
    static isBuffer(b) { return b instanceof Buffer; }
    static isEncoding(e) { return typeof e === 'string' && e.toLowerCase() in ENCODINGS; }
    static byteLength(v, encoding) {
      if (typeof v !== 'string') return v.byteLength;
      return encode(v, encoding).length;
    }
    static concat(list, total) {
      if (total === undefined) total = list.reduce((n, b) => n + b.length, 0);
      const out = Buffer.alloc(total);
      let at = 0;
      for (const b of list) { if (at >= total) break; out.set(b.subarray(0, total - at), at); at += b.length; }
      return out;
    }
    static compare(a, b) { return a.compare(b); }
    get parent() { return this.buffer; }
    get offset() { return this.byteOffset; }
    toString(encoding, start = 0, end = this.length) {
      start = Math.max(0, start | 0); end = Math.min(this.length, end === undefined ? this.length : end | 0);
      return end <= start ? '' : decode(this.subarray(start, end), encoding);
    }
    toJSON() { return { type: 'Buffer', data: Array.from(this) }; }
    equals(other) { return this.compare(other) === 0; }
    compare(other, tStart = 0, tEnd = other.length, sStart = 0, sEnd = this.length) {
      const a = this.subarray(sStart, sEnd);
      const b = other.subarray(tStart, tEnd);
      const n = Math.min(a.length, b.length);
      for (let i = 0; i < n; i++) if (a[i] !== b[i]) return a[i] < b[i] ? -1 : 1;
      return a.length === b.length ? 0 : a.length < b.length ? -1 : 1;
    }
    copy(target, tStart = 0, sStart = 0, sEnd = this.length) {
      const src = this.subarray(sStart, Math.min(sEnd, sStart + target.length - tStart));
      target.set(src, tStart);
      return src.length;
    }
    slice(start, end) { return this.subarray(start, end); }
    write(string, offset = 0, length, encoding) {
      if (typeof offset === 'string') { encoding = offset; offset = 0; length = undefined; } else if (typeof length === 'string') { encoding = length; length = undefined; }
      const bytes = encode(string, encoding);
      const n = Math.min(bytes.length, this.length - offset, length === undefined ? Infinity : length);
      this.set(bytes.subarray(0, n), offset);
      return n;
    }
    fill(value, start = 0, end = this.length, encoding) {
      if (typeof start === 'string') { encoding = start; start = 0; end = this.length; }
      if (typeof value === 'string') {
        const bytes = encode(value, encoding);
        if (bytes.length === 0) return Uint8Array.prototype.fill.call(this, 0, start, end);
        for (let i = start, j = 0; i < end; i++, j = (j + 1) % bytes.length) this[i] = bytes[j];
        return this;
      }
      if (ArrayBuffer.isView(value)) { for (let i = start, j = 0; i < end; i++, j = (j + 1) % value.length) this[i] = value[j]; return this; }
      return Uint8Array.prototype.fill.call(this, typeof value === 'boolean' ? +value : value & 255, start, end);
    }
    indexOf(value, from = 0, encoding) {
      if (typeof value === 'number') return Uint8Array.prototype.indexOf.call(this, value & 255, from);
      const needle = typeof value === 'string' ? encode(value, encoding) : value;
      if (from < 0) from = Math.max(0, this.length + from);
      outer: for (let i = from; i <= this.length - needle.length; i++) {
        for (let j = 0; j < needle.length; j++) if (this[i + j] !== needle[j]) continue outer;
        return i;
      }
      return -1;
    }
    lastIndexOf(value, from = this.length, encoding) {
      const needle = typeof value === 'number' ? [value & 255] : typeof value === 'string' ? encode(value, encoding) : value;
      outer: for (let i = Math.min(from, this.length - needle.length); i >= 0; i--) {
        for (let j = 0; j < needle.length; j++) if (this[i + j] !== needle[j]) continue outer;
        return i;
      }
      return -1;
    }
    includes(value, from, encoding) { return this.indexOf(value, from, encoding) !== -1; }
    swap16() { for (let i = 0; i + 1 < this.length; i += 2) [this[i], this[i + 1]] = [this[i + 1], this[i]]; return this; }
    swap32() { for (let i = 0; i + 3 < this.length; i += 4) { [this[i], this[i + 3]] = [this[i + 3], this[i]]; [this[i + 1], this[i + 2]] = [this[i + 2], this[i + 1]]; } return this; }
    [Symbol.for('nodejs.util.inspect.custom')]() {
      const shown = Array.from(this.subarray(0, 50), (b) => b.toString(16).padStart(2, '0')).join(' ');
      return '<Buffer' + (shown ? ' ' + shown : '') + (this.length > 50 ? ' ... ' + (this.length - 50) + ' more bytes' : '') + '>';
    }
  }

  // readUInt8 / readInt16LE / writeDoubleBE …: one DataView accessor per type and byte order
  const TYPES = [['UInt8', 'Uint8', 1], ['Int8', 'Int8', 1], ['UInt16', 'Uint16', 2], ['Int16', 'Int16', 2], ['UInt32', 'Uint32', 4], ['Int32', 'Int32', 4], ['Float', 'Float32', 4], ['Double', 'Float64', 8], ['BigUInt64', 'BigUint64', 8], ['BigInt64', 'BigInt64', 8]];
  const view = (b) => new DataView(b.buffer, b.byteOffset, b.byteLength);
  for (const [name, dv, size] of TYPES) {
    for (const order of size === 1 ? [''] : ['LE', 'BE']) {
      const little = order === 'LE';
      Buffer.prototype['read' + name + order] = function (offset = 0) { range('offset', offset, this.length - size); return view(this)['get' + dv](offset, little); };
      Buffer.prototype['write' + name + order] = function (value, offset = 0) { range('offset', offset, this.length - size); view(this)['set' + dv](offset, value, little); return offset + size; };
      if (name.startsWith('UInt')) {
        Buffer.prototype['readUint' + name.slice(4) + order] = Buffer.prototype['read' + name + order];
        Buffer.prototype['writeUint' + name.slice(4) + order] = Buffer.prototype['write' + name + order];
      }
    }
  }

  const constants = { MAX_LENGTH: 2 ** 32, MAX_STRING_LENGTH: 2 ** 29 - 24 };
  native.Buffer = Buffer;
  native.define('buffer', () => ({
    Buffer, Blob, File, atob, btoa, constants, kMaxLength: constants.MAX_LENGTH, SlowBuffer: Buffer,
    isUtf8: (b) => codec.utf8Decode(b, true) !== null,
    isAscii: (b) => b.every((x) => x < 128),
  }));
})(globalThis.__j8_native);
