// Encoding: TextEncoder / TextDecoder (UTF-8, UTF-16LE, Latin-1), atob / btoa.
(function (native) {
  'use strict';
  const { codec } = native;
  const LABELS = { 'utf-8': 'utf-8', utf8: 'utf-8', 'unicode-1-1-utf-8': 'utf-8', 'utf-16le': 'utf-16le', 'utf-16': 'utf-16le', latin1: 'windows-1252', 'iso-8859-1': 'windows-1252', 'windows-1252': 'windows-1252', ascii: 'windows-1252', 'us-ascii': 'windows-1252' };

  function bytesOf(input) {
    if (input === undefined) return new Uint8Array(0);
    if (input instanceof ArrayBuffer || (typeof SharedArrayBuffer === 'function' && input instanceof SharedArrayBuffer)) return new Uint8Array(input);
    if (ArrayBuffer.isView(input)) return new Uint8Array(input.buffer, input.byteOffset, input.byteLength);
    throw new TypeError('The "input" argument must be an ArrayBuffer or ArrayBufferView');
  }

  class TextEncoder {
    get encoding() { return 'utf-8'; }
    encode(input = '') { return codec.utf8Encode(String(input)); }
    encodeInto(source, destination) {
      const bytes = codec.utf8Encode(String(source));
      // Only whole characters: back off from a cut in the middle of a sequence
      let n = Math.min(bytes.length, destination.length);
      while (n < bytes.length && n > 0 && (bytes[n] & 0xc0) === 0x80) n--;
      destination.set(bytes.subarray(0, n));
      const read = codec.utf8Decode(bytes.subarray(0, n)).length;
      return { read, written: n };
    }
  }

  class TextDecoder {
    #encoding;
    #fatal;
    #ignoreBOM;
    constructor(label = 'utf-8', options = {}) {
      const enc = LABELS[String(label).trim().toLowerCase()];
      if (!enc) throw new RangeError('The "' + label + '" encoding is not supported');
      this.#encoding = enc;
      this.#fatal = !!options.fatal;
      this.#ignoreBOM = !!options.ignoreBOM;
    }
    get encoding() { return this.#encoding; }
    get fatal() { return this.#fatal; }
    get ignoreBOM() { return this.#ignoreBOM; }
    decode(input, options) {
      let bytes = bytesOf(input);
      let out;
      if (this.#encoding === 'utf-8') {
        if (!this.#ignoreBOM && bytes[0] === 0xef && bytes[1] === 0xbb && bytes[2] === 0xbf) bytes = bytes.subarray(3);
        out = codec.utf8Decode(bytes, this.#fatal);
        if (out === null) throw new TypeError('The encoded data was not valid for encoding utf-8');
      } else if (this.#encoding === 'utf-16le') {
        let s = '';
        for (let i = 0; i + 1 < bytes.length; i += 2) s += String.fromCharCode(bytes[i] | (bytes[i + 1] << 8));
        out = !this.#ignoreBOM && s.charCodeAt(0) === 0xfeff ? s.slice(1) : s;
      } else {
        out = '';
        for (let i = 0; i < bytes.length; i += 4096) out += String.fromCharCode.apply(null, bytes.subarray(i, i + 4096));
      }
      return out;
    }
  }

  function btoa(data) {
    if (arguments.length === 0) throw new TypeError('1 argument required');
    const s = String(data);
    const bytes = new Uint8Array(s.length);
    for (let i = 0; i < s.length; i++) {
      const c = s.charCodeAt(i);
      if (c > 0xff) throw new DOMException('Invalid character', 'InvalidCharacterError');
      bytes[i] = c;
    }
    return codec.base64Encode(bytes);
  }

  function atob(data) {
    if (arguments.length === 0) throw new TypeError('1 argument required');
    const s = String(data).replace(/[\t\n\f\r ]/g, '');
    const bytes = /^[A-Za-z0-9+/]*={0,2}$/.test(s) && s.length % 4 !== 1 ? codec.base64Decode(s) : null;
    if (bytes === null) throw new DOMException('The string to be decoded is not correctly encoded.', 'InvalidCharacterError');
    let out = '';
    for (let i = 0; i < bytes.length; i += 4096) out += String.fromCharCode.apply(null, bytes.subarray(i, i + 4096));
    return out;
  }

  for (const [name, value] of Object.entries({ TextEncoder, TextDecoder, btoa, atob })) {
    Object.defineProperty(globalThis, name, { value, writable: true, configurable: true });
  }
})(globalThis.__j8_native);
