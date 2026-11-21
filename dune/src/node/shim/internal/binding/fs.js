'use strict';
// internalBinding('fs') (src/node_file.cc) on dune's `uvfs` natives: every call is a `Call` for Rust; without a request
// it runs synchronously and throws, with an FSReqCallback it runs on a thread and calls `oncomplete`, with kUsePromises
// it returns a promise. Errors are Node's UVException, built from the errno, syscall and paths Rust reports.
const { uvfs } = globalThis.__j8_native;

const kUsePromises = Symbol('fs_use_promises_symbol');
const kFsStatsFieldsNumber = 18;
const kStatFsFieldsNumber = 7;
const ERRNO_ENOENT = -2;
const ERRNO_ENOTDIR = -20;
const ERRNO_ENOSYS = -38;

let UVException;
const uvError = ([errno, syscall, path, dest]) => {
  UVException ??= globalThis.__j8_native.requireLib('internal/errors').UVException;
  const ctx = { errno, syscall };
  if (path !== undefined && path !== null) ctx.path = path;
  if (dest !== undefined && dest !== null) ctx.dest = dest;
  return new UVException(ctx);
};

class FSReqCallback {
  constructor(bigint) {
    this.bigint = Boolean(bigint);
    this.oncomplete = undefined;
    this.context = undefined;
  }
}

class FileHandle {
  #fd;
  constructor(fd) { this.#fd = fd; }
  get fd() { return this.#fd; }
  getAsyncId() { return -1; }
  close() {
    const fd = this.#fd;
    this.#fd = -1;
    return run({ op: 'close', fd }, undefined, kUsePromises);
  }
  closeSync() {
    const fd = this.#fd;
    this.#fd = -1;
    run({ op: 'close', fd });
  }
  releaseFD() {
    const fd = this.#fd;
    this.#fd = -1;
    return fd;
  }
}

class StatWatcher {
  constructor() { this.onchange = undefined; }
  start() { return ERRNO_ENOSYS; }
  close() {}
  ref() { return this; }
  unref() { return this; }
}

const pathOf = (p) => (typeof p === 'string' ? p : ArrayBuffer.isView(p) ? Buffer.from(p.buffer, p.byteOffset, p.byteLength).toString() : String(p));
const bytesOf = (view, offset = 0, length = view.byteLength - offset) => new Uint8Array(view.buffer, view.byteOffset + offset, length);
const position = (p) => (typeof p === 'bigint' ? Number(p) : typeof p === 'number' && p >= 0 ? p : -1);

/** Runs `call` in the mode `req` asks for, mapping a successful reply through `then` */
function run(call, data, req, then = (v) => v) {
  if (req === undefined) {
    const outcome = uvfs.callSync(call, data);
    if (outcome.length !== 1) throw uvError(outcome);
    return then(outcome[0]);
  }
  const pending = uvfs.call(call, data).then((outcome) => {
    if (outcome.length !== 1) throw uvError(outcome);
    return then(outcome[0]);
  });
  if (req === kUsePromises) return pending;
  pending.then(
    (value) => { if (value === undefined) req.oncomplete(null); else req.oncomplete(null, value); },
    (err) => req.oncomplete(err),
  );
  return undefined;
}

function stats(fields, bigint) {
  if (!bigint) return new Float64Array(fields);
  const out = new BigInt64Array(kFsStatsFieldsNumber);
  for (let i = 0; i < kFsStatsFieldsNumber; i++) out[i] = BigInt(Math.trunc(fields[i]));
  return out;
}

function statfs(fields, bigint) {
  if (!bigint) return new Float64Array(fields);
  return BigInt64Array.from(fields, (f) => BigInt(Math.trunc(f)));
}

function encoded(text, encoding) {
  return encoding === 'buffer' ? Buffer.from(text) : text;
}

function statLike(op, key, target, bigint, req, throwIfNoEntry) {
  const call = { op, [key]: target };
  if (req === undefined && throwIfNoEntry === false) {
    const outcome = uvfs.callSync(call);
    if (outcome.length === 1) return stats(outcome[0], bigint);
    if (outcome[0] === ERRNO_ENOENT || outcome[0] === ERRNO_ENOTDIR) return undefined;
    throw uvError(outcome);
  }
  return run(call, undefined, req, (f) => stats(f, req instanceof FSReqCallback ? req.bigint || bigint : bigint));
}

function readInto(buffer, offset) {
  return (bytes) => {
    bytesOf(buffer).set(bytes, offset);
    return bytes.length;
  };
}

module.exports = {
  kUsePromises,
  kFsStatsFieldsNumber,
  statValues: new Float64Array(kFsStatsFieldsNumber * 2),
  bigintStatValues: new BigInt64Array(kFsStatsFieldsNumber * 2),
  statFsValues: new Float64Array(kStatFsFieldsNumber),
  bigintStatFsValues: new BigInt64Array(kStatFsFieldsNumber),
  FSReqCallback,
  FileHandle,
  StatWatcher,

  access: (path, mode, req) => run({ op: 'access', path: pathOf(path), mode }, undefined, req),
  close: (fd, req) => run({ op: 'close', fd }, undefined, req),
  existsSync: (path) => uvfs.exists(pathOf(path)),
  open: (path, flags, mode, req) => run({ op: 'open', path: pathOf(path), flags, mode }, undefined, req),
  openFileHandle: (path, flags, mode, req) => run({ op: 'open', path: pathOf(path), flags, mode }, undefined, req, (fd) => new FileHandle(fd)),
  read(fd, buffer, offset, length, pos, req) {
    return run({ op: 'read', fd, length, position: position(pos) }, undefined, req, readInto(buffer, offset));
  },
  readBuffers(fd, buffers, pos, req) {
    const total = buffers.reduce((n, b) => n + b.byteLength, 0);
    return run({ op: 'read', fd, length: total, position: position(pos) }, undefined, req, (bytes) => {
      let at = 0;
      for (const b of buffers) {
        if (at >= bytes.length) break;
        const part = bytes.subarray(at, at + b.byteLength);
        bytesOf(b).set(part);
        at += part.length;
      }
      return bytes.length;
    });
  },
  readFileUtf8(target, flags) {
    const t = typeof target === 'number' ? target : pathOf(target);
    return run({ op: 'readFile', target: t, flags }, undefined, undefined, (bytes) => Buffer.from(bytes.buffer, bytes.byteOffset, bytes.length).toString('utf8'));
  },
  fdatasync: (fd, req) => run({ op: 'fdatasync', fd }, undefined, req),
  fsync: (fd, req) => run({ op: 'fsync', fd }, undefined, req),
  rename: (from, to, req) => run({ op: 'rename', from: pathOf(from), to: pathOf(to) }, undefined, req),
  ftruncate: (fd, length, req) => run({ op: 'ftruncate', fd, length: Number(length) }, undefined, req),
  rmdir: (path, req) => run({ op: 'rmdir', path: pathOf(path) }, undefined, req),
  rmSync: (path, maxRetries, recursive) => run({ op: 'rm', path: pathOf(path), recursive: Boolean(recursive) }),
  mkdir: (path, mode, recursive, req) => run({ op: 'mkdir', path: pathOf(path), mode, recursive: Boolean(recursive) }, undefined, req),
  readdir(path, encoding, withFileTypes, req) {
    return run({ op: 'readdir', path: pathOf(path) }, undefined, req, ([names, types]) => {
      const out = names.map((n) => encoded(n, encoding));
      return withFileTypes ? [out, types] : out;
    });
  },
  internalModuleStat: (receiver, path) => uvfs.moduleStat(pathOf(path === undefined ? receiver : path)),
  stat: (path, bigint, req, throwIfNoEntry) => statLike('stat', 'path', pathOf(path), bigint, req, throwIfNoEntry),
  lstat: (path, bigint, req, throwIfNoEntry) => statLike('lstat', 'path', pathOf(path), bigint, req, throwIfNoEntry),
  fstat: (fd, bigint, req, shouldNotThrow) => statLike('fstat', 'fd', fd, bigint, req, shouldNotThrow === true ? false : undefined),
  statfs: (path, bigint, req) => run({ op: 'statfs', path: pathOf(path) }, undefined, req, (f) => statfs(f, bigint)),
  link: (from, to, req) => run({ op: 'link', from: pathOf(from), to: pathOf(to) }, undefined, req),
  symlink: (target, path, flags, req) => run({ op: 'symlink', target: pathOf(target), path: pathOf(path) }, undefined, req),
  readlink: (path, encoding, req) => run({ op: 'readlink', path: pathOf(path) }, undefined, req, (t) => encoded(t, encoding)),
  unlink: (path, req) => run({ op: 'unlink', path: pathOf(path) }, undefined, req),
  writeBuffer: (fd, buffer, offset, length, pos, req) => run({ op: 'write', fd, position: position(pos) }, bytesOf(buffer, offset, length), req),
  writeBuffers(fd, buffers, pos, req) {
    const total = buffers.reduce((n, b) => n + b.byteLength, 0);
    const data = new Uint8Array(total);
    let at = 0;
    for (const b of buffers) { data.set(bytesOf(b), at); at += b.byteLength; }
    return run({ op: 'write', fd, position: position(pos) }, data, req);
  },
  writeString: (fd, string, pos, encoding, req) => run({ op: 'write', fd, position: position(pos) }, Buffer.from(String(string), encoding || 'utf8'), req),
  writeFileUtf8(target, data, flags, mode) {
    const t = typeof target === 'number' ? target : pathOf(target);
    return run({ op: 'writeFile', target: t, flags, mode }, Buffer.from(String(data), 'utf8'));
  },
  realpath: (path, encoding, req) => run({ op: 'realpath', path: pathOf(path) }, undefined, req, (t) => encoded(t, encoding)),
  copyFile: (from, to, mode, req) => run({ op: 'copyFile', from: pathOf(from), to: pathOf(to), mode: mode | 0 }, undefined, req),
  chmod: (path, mode, req) => run({ op: 'chmod', path: pathOf(path), mode }, undefined, req),
  fchmod: (fd, mode, req) => run({ op: 'fchmod', fd, mode }, undefined, req),
  chown: (path, uid, gid, req) => run({ op: 'chown', path: pathOf(path), uid, gid }, undefined, req),
  fchown: (fd, uid, gid, req) => run({ op: 'fchown', fd, uid, gid }, undefined, req),
  lchown: (path, uid, gid, req) => run({ op: 'lchown', path: pathOf(path), uid, gid }, undefined, req),
  utimes: (path, atime, mtime, req) => run({ op: 'utimes', path: pathOf(path), atime, mtime }, undefined, req),
  futimes: (fd, atime, mtime, req) => run({ op: 'futimes', fd, atime, mtime }, undefined, req),
  lutimes: (path, atime, mtime, req) => run({ op: 'lutimes', path: pathOf(path), atime, mtime }, undefined, req),
  mkdtemp: (prefix, encoding, req) => run({ op: 'mkdtemp', prefix: pathOf(prefix) }, undefined, req, (t) => encoded(t, encoding)),
  cpSyncCheckPaths() { throw uvError([ERRNO_ENOSYS, 'cp']); },
  cpSyncOverrideFile() { throw uvError([ERRNO_ENOSYS, 'cp']); },
  cpSyncCopyDir() { throw uvError([ERRNO_ENOSYS, 'cp']); },
  legacyMainResolve() { throw uvError([ERRNO_ENOSYS, 'legacyMainResolve']); },
  getFormatOfExtensionlessFile: () => 0,
};
