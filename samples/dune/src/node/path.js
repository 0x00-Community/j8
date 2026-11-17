// node:path — POSIX paths.
(function (native) {
  'use strict';
  const str = (p, name = 'path') => {
    if (typeof p !== 'string') throw new TypeError('The "' + name + '" argument must be of type string. Received ' + native.inspect(p));
    return p;
  };

  function normalizeParts(path, absolute) {
    const out = [];
    for (const seg of path.split('/')) {
      if (seg === '' || seg === '.') continue;
      if (seg === '..') { if (out.length && out[out.length - 1] !== '..') out.pop(); else if (!absolute) out.push('..'); } else out.push(seg);
    }
    return out.join('/');
  }

  const posix = {
    sep: '/',
    delimiter: ':',
    normalize(p) {
      str(p);
      if (p === '') return '.';
      const absolute = p.startsWith('/');
      const trailing = p.endsWith('/');
      let out = normalizeParts(p, absolute);
      if (out === '' && !absolute) out = '.';
      if (out !== '' && trailing) out += '/';
      return (absolute ? '/' : '') + out;
    },
    join(...parts) {
      const joined = parts.map((p) => str(p)).filter((p) => p !== '').join('/');
      return joined === '' ? '.' : posix.normalize(joined);
    },
    resolve(...parts) {
      let out = '';
      for (let i = parts.length - 1; i >= -1 && !out.startsWith('/'); i--) {
        const p = i >= 0 ? str(parts[i]) : process.cwd();
        if (p !== '') out = p + (out ? '/' + out : '');
      }
      return '/' + normalizeParts(out, true);
    },
    isAbsolute: (p) => str(p).startsWith('/'),
    dirname(p) {
      str(p);
      if (p === '') return '.';
      const trimmed = p.replace(/\/+$/, '') || '/';
      if (trimmed === '/') return '/';
      const i = trimmed.lastIndexOf('/');
      if (i < 0) return '.';
      return trimmed.slice(0, i).replace(/\/+$/, '') || '/';
    },
    basename(p, ext) {
      str(p);
      const base = p.replace(/\/+$/, '').split('/').pop() || '';
      return ext !== undefined && base.endsWith(ext) && base !== ext ? base.slice(0, base.length - ext.length) : base;
    },
    extname(p) {
      const base = posix.basename(p);
      const i = base.lastIndexOf('.');
      return i <= 0 ? '' : base.slice(i);
    },
    relative(from, to) {
      const a = posix.resolve(str(from, 'from')).split('/').filter(Boolean);
      const b = posix.resolve(str(to, 'to')).split('/').filter(Boolean);
      let i = 0;
      while (i < a.length && i < b.length && a[i] === b[i]) i++;
      return [...Array(a.length - i).fill('..'), ...b.slice(i)].join('/');
    },
    parse(p) {
      str(p);
      const root = p.startsWith('/') ? '/' : '';
      const base = posix.basename(p);
      const ext = posix.extname(p);
      let dir = posix.dirname(p);
      if (dir === '.' && !p.startsWith('.')) dir = '';
      return { root, dir, base, ext, name: ext ? base.slice(0, -ext.length) : base };
    },
    format(o) {
      const dir = o.dir || o.root || '';
      const base = o.base || (o.name || '') + (o.ext ? (o.ext.startsWith('.') ? '' : '.') + o.ext : '');
      return dir ? (dir === o.root ? dir + base : dir + '/' + base) : base;
    },
    toNamespacedPath: (p) => p,
    matchesGlob(p, glob) {
      const re = '^' + String(glob).replace(/[.+^${}()|[\]\\]/g, '\\$&').replace(/\*\*\/?/g, '\u0000').replace(/\*/g, '[^/]*').replace(/\?/g, '[^/]').replace(/\u0000/g, '(?:.*/)?') + '$';
      return new RegExp(re).test(str(p));
    },
  };
  posix.posix = posix;
  native.define('path', () => posix);
  native.define('path/posix', () => posix);
})(globalThis.__j8_native);
