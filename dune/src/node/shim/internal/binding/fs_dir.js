'use strict';
// internalBinding('fs_dir') (src/node_dir.cc): a directory handle reading entries in batches of `bufferSize`; the
// listing is taken when the directory is opened
const fs = globalThis.__j8_native.requireLib('internal/binding/fs');

const settle = (req, error, value) => {
  queueMicrotask(() => {
    if (error) req.oncomplete(error);
    else if (value === undefined) req.oncomplete(null);
    else req.oncomplete(null, value);
  });
};

class DirHandle {
  #entries;
  #at = 0;
  constructor(names, types) { this.#entries = names.map((n, i) => [n, types[i]]); }
  read(encoding, bufferSize, req) {
    const batch = this.#entries.slice(this.#at, this.#at + bufferSize);
    this.#at += batch.length;
    const out = batch.length === 0 ? null : batch.flatMap(([n, t]) => [encoding === 'buffer' ? Buffer.from(n) : n, t]);
    if (req === undefined) return out;
    settle(req, null, out);
    return undefined;
  }
  close(req) {
    this.#entries = [];
    if (req !== undefined) settle(req);
  }
}

function open(path) {
  const [names, types] = fs.readdir(path, 'utf8', true);
  return new DirHandle(names, types);
}

module.exports = {
  opendir(path, encoding, req) {
    if (req === undefined) return open(path);
    try { settle(req, null, open(path)); } catch (e) { settle(req, e); }
    return undefined;
  },
  opendirSync: (path) => open(path),
};
