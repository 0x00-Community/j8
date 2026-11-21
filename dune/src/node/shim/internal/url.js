'use strict';
// internal/url on the Web layer's URL (Node's is a C++ parser, ada); the file-URL helpers follow Node's POSIX branch
const { URL, URLSearchParams } = globalThis;
const {
  ERR_INVALID_ARG_TYPE,
  ERR_INVALID_FILE_URL_HOST,
  ERR_INVALID_FILE_URL_PATH,
  ERR_INVALID_URL_SCHEME,
} = require('internal/errors').codes;
const { validateObject, kValidateObjectAllowObjects } = require('internal/validators');

const unsafeProtocol = new Set(['javascript', 'javascript:']);
const hostlessProtocol = new Set(['javascript', 'javascript:']);
const slashedProtocol = new Set(['http', 'http:', 'https', 'https:', 'ftp', 'ftp:', 'gopher', 'gopher:', 'file', 'file:', 'ws', 'ws:', 'wss', 'wss:']);

function isURL(self) {
  return Boolean(self?.href && self.protocol && self.auth === undefined && self.path === undefined);
}

function fileURL(path) {
  if (typeof path === 'string') path = new URL(path);
  else if (!isURL(path)) throw new ERR_INVALID_ARG_TYPE('path', ['string', 'URL'], path);
  if (path.protocol !== 'file:') throw new ERR_INVALID_URL_SCHEME('file');
  if (path.hostname !== '') throw new ERR_INVALID_FILE_URL_HOST(process.platform);
  return path;
}

function fileURLToPath(path) {
  const url = fileURL(path);
  const pathname = url.pathname;
  if (/%2f/i.test(pathname)) throw new ERR_INVALID_FILE_URL_PATH('must not include encoded / characters', url);
  return pathname.includes('%') ? decodeURIComponent(pathname) : pathname;
}

function fileURLToPathBuffer(path) {
  const { Buffer } = require('buffer');
  const bytes = Buffer.from(fileURL(path).pathname, 'utf8');
  const out = [];
  for (let i = 0; i < bytes.length; i++) {
    const hex = bytes[i] === 0x25 && i + 2 < bytes.length ? String.fromCharCode(bytes[i + 1], bytes[i + 2]) : '';
    if (/^[0-9a-f]{2}$/i.test(hex)) {
      out.push(parseInt(hex, 16));
      i += 2;
    } else {
      out.push(bytes[i]);
    }
  }
  return Buffer.from(out);
}

function pathToFileURL(filepath) {
  const path = require('path');
  let resolved = path.posix.resolve(filepath);
  if (filepath.endsWith('/') && !resolved.endsWith('/')) resolved += '/';
  const encoded = resolved.replace(/[%\\\n\r\t?#]/g, (c) => '%' + c.charCodeAt(0).toString(16).toUpperCase().padStart(2, '0'));
  return new URL('file://' + encodeURI(encoded).replace(/%25([0-9A-F]{2})/g, '%$1'));
}

function toPathIfFileURL(fileURLOrPath) {
  return isURL(fileURLOrPath) ? fileURLToPath(fileURLOrPath) : fileURLOrPath;
}

function urlToHttpOptions(url) {
  validateObject(url, 'url', kValidateObjectAllowObjects);
  const { hostname, pathname, port, username, password, search } = url;
  const options = {
    __proto__: null,
    ...url,
    protocol: url.protocol,
    hostname: hostname && hostname[0] === '[' ? hostname.slice(1, -1) : hostname,
    hash: url.hash,
    search,
    pathname,
    path: `${pathname || ''}${search || ''}`,
    href: url.href,
  };
  if (port !== '') options.port = Number(port);
  if (username || password) options.auth = `${decodeURIComponent(username)}:${decodeURIComponent(password)}`;
  return options;
}

const noEscape = new Int8Array(128);
for (const c of "!'()*-._~") noEscape[c.charCodeAt(0)] = 1;
for (let c = 0x30; c <= 0x39; c++) noEscape[c] = 1;
for (let c = 0x41; c <= 0x5a; c++) noEscape[c] = 1;
for (let c = 0x61; c <= 0x7a; c++) noEscape[c] = 1;
const encodeStr = (str, table = noEscape) => {
  let out = '';
  for (const ch of str) {
    const code = ch.codePointAt(0);
    out += code < 128 && table[code] === 1 ? ch : encodeURIComponent(ch);
  }
  return out;
};

const domainToASCII = (domain) => {
  try { return new URL(`ws://${domain}`).hostname; } catch { return ''; }
};

module.exports = {
  fileURLToPath,
  fileURLToPathBuffer,
  pathToFileURL,
  toPathIfFileURL,
  installObjectURLMethods() {},
  URL,
  URLSearchParams,
  URLParse: (input, base) => { try { return new URL(input, base); } catch { return null; } },
  domainToASCII,
  domainToUnicode: (domain) => domain,
  urlToHttpOptions,
  encodeStr,
  isURL,
  urlUpdateActions: {},
  getURLOrigin: (url) => new URL(url).origin,
  unsafeProtocol,
  hostlessProtocol,
  slashedProtocol,
};
