// node:fs and node:fs/promises — the synchronous, promise and callback APIs over the native file system.
(function (native) {
  'use strict';
  const raw = native.fs;
  const { Buffer, codec } = native;
  const constants = { F_OK: 0, R_OK: 4, W_OK: 2, X_OK: 1, O_RDONLY: 0, O_WRONLY: 1, O_RDWR: 2, O_CREAT: 64, O_EXCL: 128, O_TRUNC: 512, O_APPEND: 1024, S_IFMT: 61440, S_IFREG: 32768, S_IFDIR: 16384, S_IFLNK: 40960, COPYFILE_EXCL: 1 };

  /** Adds `code` / `errno` / `syscall` / `path` to the native error (its message starts with the code) */
  function fsError(e) {
    const m = e instanceof Error && /^([A-Z]+): .*?, (\w+) '(.*)'$/.exec(e.message);
    if (m) Object.assign(e, { errno: -({ ENOENT: 2, EACCES: 13, EEXIST: 17, EISDIR: 21, ENOTDIR: 20, ENOTEMPTY: 39, EINVAL: 22 }[m[1]] || 5), code: m[1], syscall: m[2], path: m[3] });
    return e;
  }
  const sync = (f) => (...args) => { try { return f(...args); } catch (e) { throw fsError(e); } };
  const pathOf = (p) => {
    if (p instanceof URL) { if (p.protocol !== 'file:') throw new TypeError('The URL must be of scheme file'); return decodeURIComponent(p.pathname); }
    if (ArrayBuffer.isView(p)) return codec.utf8Decode(p);
    if (typeof p !== 'string') throw new TypeError('The "path" argument must be of type string or an instance of Buffer or URL. Received ' + native.inspect(p));
    return p;
  };
  const encodingOf = (o) => (typeof o === 'string' ? o : o && o.encoding ? o.encoding : null);
  const bytesOf = (data, o) => (typeof data === 'string' ? Buffer.from(data, encodingOf(o) || 'utf8') : ArrayBuffer.isView(data) ? data : Buffer.from(String(data)));
  const decoded = (bytes, o) => { const enc = encodingOf(o); const b = Buffer.wrap(bytes); return enc && enc !== 'buffer' ? b.toString(enc) : b; };

  class Stats {
    constructor(f) {
      [this._kind, this.size, this.mode, this.mtimeMs, this.atimeMs, this.ctimeMs, this.birthtimeMs, this.nlink, this.uid, this.gid, this.ino, this.dev] = f;
      this.blksize = 4096;
      this.blocks = Math.ceil(this.size / 512);
      this.rdev = 0;
      for (const k of ['mtime', 'atime', 'ctime', 'birthtime']) this[k] = new Date(this[k + 'Ms']);
      Object.defineProperty(this, '_kind', { enumerable: false });
    }
    isFile() { return this._kind === 0; }
    isDirectory() { return this._kind === 1; }
    isSymbolicLink() { return this._kind === 2; }
    isFIFO() { return false; }
    isSocket() { return false; }
    isBlockDevice() { return false; }
    isCharacterDevice() { return false; }
  }

  class Dirent {
    constructor(name, kind, parentPath) { this.name = name; this.parentPath = parentPath; this.path = parentPath; Object.defineProperty(this, '_kind', { value: kind }); }
    isFile() { return this._kind === 0; }
    isDirectory() { return this._kind === 1; }
    isSymbolicLink() { return this._kind === 2; }
    isFIFO() { return false; }
    isSocket() { return false; }
    isBlockDevice() { return false; }
    isCharacterDevice() { return false; }
  }

  function listing([names, kinds], dir, o) {
    if (o && o.withFileTypes) return names.map((n, i) => new Dirent(n, kinds[i], dir));
    const enc = encodingOf(o);
    return enc === 'buffer' ? names.map((n) => Buffer.from(n)) : names;
  }

  function walk(dir, o, out, prefix) {
    const [names, kinds] = raw.listSync(dir);
    names.forEach((n, i) => {
      const rel = prefix ? prefix + '/' + n : n;
      out.push(o && o.withFileTypes ? new Dirent(n, kinds[i], dir) : rel);
      if (kinds[i] === 1) walk(dir + '/' + n, o, out, rel);
    });
    return out;
  }

  const fs = {
    constants,
    Stats,
    Dirent,
    readFileSync: sync((p, o) => decoded(raw.readSync(pathOf(p)), o)),
    writeFileSync: sync((p, data, o) => raw.writeSync(pathOf(p), bytesOf(data, o), !!(o && o.flag && o.flag.startsWith('a')))),
    appendFileSync: sync((p, data, o) => raw.writeSync(pathOf(p), bytesOf(data, o), true)),
    existsSync: (p) => { try { return raw.exists(pathOf(p)); } catch { return false; } },
    statSync: sync((p, o) => { try { return new Stats(raw.statSync(pathOf(p), true)); } catch (e) { if (o && o.throwIfNoEntry === false && /^ENOENT/.test(e.message)) return undefined; throw e; } }),
    lstatSync: sync((p) => new Stats(raw.statSync(pathOf(p), false))),
    readdirSync: sync((p, o) => (o && o.recursive ? walk(pathOf(p), o, [], '') : listing(raw.listSync(pathOf(p)), pathOf(p), o))),
    mkdirSync: sync((p, o) => { const path = pathOf(p); const rec = !!(o && (o.recursive || o === true)); const existed = rec && raw.exists(path); raw.mkdirSync(path, rec); return rec && !existed ? path : undefined; }),
    rmSync: sync((p, o = {}) => raw.rmSync(pathOf(p), !!o.recursive, !!o.force)),
    rmdirSync: sync((p, o = {}) => (o.recursive ? raw.rmSync(pathOf(p), true, false) : raw.rmdirSync(pathOf(p)))),
    unlinkSync: sync((p) => raw.unlinkSync(pathOf(p))),
    renameSync: sync((a, b) => raw.renameSync(pathOf(a), pathOf(b))),
    copyFileSync: sync((a, b, mode) => { if (mode & constants.COPYFILE_EXCL && raw.exists(pathOf(b))) throw new Error("EEXIST: file already exists, copyfile '" + pathOf(a) + "'"); raw.copySync(pathOf(a), pathOf(b)); }),
    cpSync: sync((a, b, o = {}) => copyTree(pathOf(a), pathOf(b), o)),
    realpathSync: sync((p) => raw.realpathSync(pathOf(p))),
    readlinkSync: sync((p) => raw.readlinkSync(pathOf(p))),
    symlinkSync: sync((target, p) => raw.symlinkSync(pathOf(target), pathOf(p))),
    chmodSync: sync((p, mode) => raw.chmodSync(pathOf(p), typeof mode === 'string' ? parseInt(mode, 8) : mode)),
    mkdtempSync: sync((prefix) => raw.mkdtempSync(prefix)),
    accessSync: sync((p) => { if (!raw.exists(pathOf(p))) throw new Error("ENOENT: no such file or directory, access '" + pathOf(p) + "'"); }),
  };
  fs.realpathSync.native = fs.realpathSync;

  function copyTree(from, to, o) {
    const st = raw.statSync(from, true);
    if (st[0] === 1) {
      if (!o.recursive) throw new Error("EISDIR: illegal operation on a directory, cp '" + from + "'");
      raw.mkdirSync(to, true);
      for (const n of raw.listSync(from)[0]) copyTree(from + '/' + n, to + '/' + n, o);
    } else if (o.force !== false || !raw.exists(to)) raw.copySync(from, to);
  }

  const wrap = (p) => p.catch((e) => { throw fsError(e); });
  const promises = {
    constants,
    readFile: (p, o) => wrap(raw.readAsync(pathOf(p)).then((b) => decoded(b, o))),
    writeFile: (p, data, o) => wrap(raw.writeAsync(pathOf(p), bytesOf(data, o), !!(o && o.flag && o.flag.startsWith('a')))),
    appendFile: (p, data, o) => wrap(raw.writeAsync(pathOf(p), bytesOf(data, o), true)),
    stat: (p) => wrap(raw.statAsync(pathOf(p), true).then((f) => new Stats(f))),
    lstat: (p) => wrap(raw.statAsync(pathOf(p), false).then((f) => new Stats(f))),
    readdir: (p, o) => (o && o.recursive ? Promise.resolve().then(() => fs.readdirSync(p, o)) : wrap(raw.listAsync(pathOf(p)).then((l) => listing(l, pathOf(p), o)))),
    mkdir: (p, o) => { const rec = !!(o && o.recursive); return wrap(raw.mkdirAsync(pathOf(p), rec).then(() => (rec ? pathOf(p) : undefined))); },
    rm: (p, o = {}) => wrap(raw.rmAsync(pathOf(p), !!o.recursive, !!o.force)),
  };
  // The rest have no separate async form: run the synchronous one in a promise
  for (const [name, f] of [['rmdir', 'rmdirSync'], ['unlink', 'unlinkSync'], ['rename', 'renameSync'], ['copyFile', 'copyFileSync'], ['cp', 'cpSync'], ['realpath', 'realpathSync'], ['readlink', 'readlinkSync'], ['symlink', 'symlinkSync'], ['chmod', 'chmodSync'], ['mkdtemp', 'mkdtempSync'], ['access', 'accessSync']]) {
    promises[name] = (...args) => new Promise((resolve) => resolve(fs[f](...args)));
  }

  // The callback API: the promise form, then `cb(err, value)` on a later tick
  for (const name of Object.keys(promises)) {
    if (name === 'constants') continue;
    fs[name] = (...args) => {
      const cb = args.pop();
      if (typeof cb !== 'function') throw new TypeError('The "cb" argument must be of type function. Received ' + native.inspect(cb));
      promises[name](...args).then((v) => cb(null, v), (e) => cb(e));
    };
  }
  fs.exists = (p, cb) => { const ok = fs.existsSync(p); queueMicrotask(() => cb(ok)); };
  fs.promises = promises;

  native.define('fs', () => fs);
  native.define('fs/promises', () => promises);
})(globalThis.__j8_native);
