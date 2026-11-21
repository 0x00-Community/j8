'use strict';
// internalBinding('buffer') (src/node_buffer.cc): argument checks and errors as there, the byte work in dune's `raw`
const { raw } = globalThis.__j8_native;

const ENCODING = { ascii: 0, utf8: 1, base64: 2, ucs2: 3, utf16le: 3, latin1: 4, binary: 4, hex: 5, buffer: 6, base64url: 7 };
const UNITS_PER_CALL = 8192;

function nodeError(Base, code, message) {
  const e = new Base(message);
  Object.defineProperty(e, 'code', { value: code, writable: true, configurable: true, enumerable: true });
  return e;
}
const outOfRange = (message = 'Index out of range') => nodeError(RangeError, 'ERR_OUT_OF_RANGE', message);

function isView(value) {
  return ArrayBuffer.isView(value);
}

function assertBuffer(value) {
  if (!isView(value)) throw nodeError(TypeError, 'ERR_INVALID_ARG_TYPE', 'argument must be a buffer');
}

/** ParseArrayIndex: undefined ⇒ `def`; a negative index is out of range */
function index(value, def) {
  if (value === undefined) return def;
  const n = Math.trunc(Number(value)) || 0;
  if (n < 0) throw outOfRange();
  return n;
}

function slicer(encoding) {
  return function slice(buf, start, end) {
    assertBuffer(buf);
    const length = buf.byteLength;
    if (length === 0) return '';
    const s = index(start, 0);
    let e = index(end, length);
    if (e < s) e = s;
    if (e > length) throw outOfRange();
    if (encoding === ENCODING.ucs2) return ucs2(buf, s, e);
    return raw.decode(buf, s, e, encoding);
  };
}

function ucs2(buf, start, end) {
  const bytes = new Uint8Array(buf.buffer, buf.byteOffset + start, end - start);
  const count = bytes.length >> 1;
  let out = '';
  for (let i = 0; i < count; i += UNITS_PER_CALL) {
    const units = [];
    for (let j = i; j < Math.min(count, i + UNITS_PER_CALL); j++) units.push(bytes[2 * j] | (bytes[2 * j + 1] << 8));
    out += String.fromCharCode.apply(null, units);
  }
  return out;
}

function writer(encoding) {
  return function write(buf, string, offset, length) {
    assertBuffer(buf);
    if (typeof string !== 'string') throw nodeError(TypeError, 'ERR_INVALID_ARG_TYPE', 'argument must be a string');
    const total = buf.byteLength;
    const at = index(offset, 0);
    if (at > total) throw nodeError(RangeError, 'ERR_BUFFER_OUT_OF_BOUNDS', '"offset" is outside of buffer bounds');
    const max = Math.min(total - at, index(length, total - at));
    if (max === 0) return 0;
    return raw.write(buf, string, at, max, encoding);
  };
}

function indexOfOffset(length, offset, needleLength, forward) {
  if (offset < 0) {
    if (offset + length >= 0) return length + offset;
    if (forward || needleLength === 0) return 0;
    return -1;
  }
  if (offset + needleLength <= length) return offset;
  if (needleLength === 0) return length;
  if (forward) return -1;
  return length - 1;
}

function search(haystack, needle, byteOffset, wide, forward) {
  const length = wide ? haystack.byteLength & ~1 : haystack.byteLength;
  const at = indexOfOffset(length, Math.trunc(byteOffset), needle.length, forward);
  if (needle.length === 0) return at;
  if (length === 0 || at <= -1) return -1;
  if ((forward && needle.length + at > length) || needle.length > length) return -1;
  if (wide && (length < 2 || needle.length < 2)) return -1;
  return raw.indexOf(haystack, needle, at, forward, wide);
}

function indexOfString(buffer, needle, byteOffset, encoding, forward) {
  assertBuffer(buffer);
  if (![ENCODING.ucs2, ENCODING.utf8, ENCODING.ascii, ENCODING.latin1].includes(encoding)) return -1;
  return search(buffer, raw.encode(needle, encoding), byteOffset, encoding === ENCODING.ucs2, forward);
}

function indexOfBuffer(haystack, needle, byteOffset, encoding, forward) {
  assertBuffer(haystack);
  assertBuffer(needle);
  const bytes = new Uint8Array(needle.buffer, needle.byteOffset, needle.byteLength);
  return search(haystack, bytes, byteOffset, encoding === ENCODING.ucs2, forward);
}

function indexOfNumber(buffer, needle, byteOffset, forward) {
  assertBuffer(buffer);
  const length = buffer.byteLength;
  const at = indexOfOffset(length, Math.trunc(byteOffset), 1, forward);
  if (at <= -1 || length === 0) return -1;
  return raw.indexOf(buffer, new Uint8Array([needle & 0xff]), at, forward, false);
}

function fill(buf, value, start, end, encoding) {
  assertBuffer(buf);
  const s = index(start, 0);
  const e = index(end, 0);
  if (s > e || e > buf.byteLength) return -2;
  if (s === e) return undefined;
  let pattern;
  if (isView(value)) {
    pattern = new Uint8Array(value.buffer, value.byteOffset, value.byteLength);
  } else if (typeof value !== 'string') {
    pattern = new Uint8Array([Number(value) & 0xff]);
  } else {
    const code = ENCODING[encoding] ?? ENCODING.utf8;
    pattern = raw.encode(value, code);
  }
  return raw.fill(buf, pattern, s, e) ? undefined : -1;
}

function compareOffset(source, target, targetStart, sourceStart, targetEnd, sourceEnd) {
  assertBuffer(source);
  assertBuffer(target);
  const ts = index(targetStart, 0);
  const ss = index(sourceStart, 0);
  const te = index(targetEnd, target.byteLength);
  const se = index(sourceEnd, source.byteLength);
  if (ss > source.byteLength) throw outOfRange('The value of "sourceStart" is out of range.');
  if (ts > target.byteLength) throw outOfRange('The value of "targetStart" is out of range.');
  const count = Math.min(se - ss, te - ts, source.byteLength - ss);
  const head = raw.compare(source, target, ss, ss + count, ts, ts + count);
  if (head !== 0) return head;
  return Math.sign((se - ss) - (te - ts));
}

function compare(a, b) {
  assertBuffer(a);
  assertBuffer(b);
  return raw.compare(a, b, 0, a.byteLength, 0, b.byteLength);
}

function swapper(width) {
  return (buf) => { raw.swap(buf, width); return buf; };
}

module.exports = {
  kMaxLength: 2 ** 53 - 1,
  kStringMaxLength: 536870888,
  atob: globalThis.atob,
  btoa: globalThis.btoa,
  byteLengthUtf8: (string) => raw.utf8Length(string),
  copy: (source, target, targetStart, sourceStart, count) => raw.copy(source, target, targetStart, sourceStart, count),
  compare,
  compareOffset,
  fill,
  indexOfBuffer,
  indexOfNumber,
  indexOfString,
  copyArrayBuffer(destination, destinationOffset, source, sourceOffset, bytes) {
    new Uint8Array(destination, destinationOffset, bytes).set(new Uint8Array(source, sourceOffset, bytes));
  },
  createUnsafeArrayBuffer: (size) => new ArrayBuffer(size),
  swap16: swapper(2),
  swap32: swapper(4),
  swap64: swapper(8),
  isUtf8: (view) => raw.isUtf8(view),
  isAscii: (view) => raw.isAscii(view),
  asciiSlice: slicer(ENCODING.ascii),
  base64Slice: slicer(ENCODING.base64),
  base64urlSlice: slicer(ENCODING.base64url),
  latin1Slice: slicer(ENCODING.latin1),
  hexSlice: slicer(ENCODING.hex),
  ucs2Slice: slicer(ENCODING.ucs2),
  utf8Slice: slicer(ENCODING.utf8),
  asciiWriteStatic: writer(ENCODING.ascii),
  base64Write: writer(ENCODING.base64),
  base64urlWrite: writer(ENCODING.base64url),
  hexWrite: writer(ENCODING.hex),
  latin1WriteStatic: writer(ENCODING.latin1),
  ucs2Write: writer(ENCODING.ucs2),
  utf8WriteStatic: writer(ENCODING.utf8),
  setBufferPrototype() {},
  setDetachKey() {},
  getZeroFillToggle: () => undefined,
};
