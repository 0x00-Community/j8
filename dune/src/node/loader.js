// The module system: built-in module registry (`native.define` / `native.builtin`), CommonJS `require` with Node's
// resolution (relative paths, node_modules, package.json `main` / `exports`, `.js` / `.json` / `.cjs`, directory
// index), and the entry point `native.runMain` (CommonJS, or an ES module by extension / package.json `type`).
(function (native) {
  'use strict';
  const { fs, codec } = native;
  const factories = Object.create(null);
  const builtins = Object.create(null);

  native.define = (name, factory) => { factories[name] = factory; };
  native.builtin = (name) => {
    if (!(name in builtins)) {
      if (!(name in factories)) throw new Error('No such built-in module: node:' + name);
      builtins[name] = factories[name]();
    }
    return builtins[name];
  };
  native.builtinNames = () => Object.keys(factories);
  const builtinName = (request) => {
    const name = request.startsWith('node:') ? request.slice(5) : request;
    return name in factories ? name : null;
  };

  const path = () => native.builtin('path');
  const text = (file) => {
    const s = codec.utf8Decode(fs.readSync(file));
    return s.charCodeAt(0) === 0xfeff ? s.slice(1) : s;
  };
  const isFile = (p) => { try { return fs.statSync(p, true)[0] === 0; } catch { return false; } };
  const isDir = (p) => { try { return fs.statSync(p, true)[0] === 1; } catch { return false; } };
  const readJson = (p) => { try { return JSON.parse(text(p)); } catch { return null; } };

  const EXTENSIONS = ['.js', '.json', '.cjs', '.mjs'];

  function asFile(p) {
    if (isFile(p)) return p;
    for (const ext of EXTENSIONS) if (isFile(p + ext)) return p + ext;
    return null;
  }

  /** The target of package.json `exports` for `sub` ('.' or './x'), preferring require / node / default */
  function exportTarget(exports, sub) {
    if (typeof exports === 'string' || Array.isArray(exports)) return sub === '.' ? [].concat(exports)[0] : null;
    if (!exports || typeof exports !== 'object') return null;
    const keys = Object.keys(exports);
    let entry = keys.length && !keys[0].startsWith('.') ? (sub === '.' ? exports : undefined) : exports[sub];
    if (entry === undefined) {
      for (const k of keys) {
        const star = k.indexOf('*');
        if (star >= 0 && sub.startsWith(k.slice(0, star)) && sub.endsWith(k.slice(star + 1))) {
          const hole = sub.slice(star, sub.length - (k.length - star - 1));
          const t = pick(exports[k]);
          return t && t.replace(/\*/g, hole);
        }
      }
      return null;
    }
    return pick(entry);
  }

  function pick(entry) {
    while (entry && typeof entry === 'object') {
      if (Array.isArray(entry)) { entry = entry[0]; continue; }
      const k = ['require', 'node', 'default'].find((c) => c in entry);
      if (!k) return null;
      entry = entry[k];
    }
    return typeof entry === 'string' ? entry : null;
  }

  function asDirectory(p) {
    const pkg = readJson(path().join(p, 'package.json'));
    if (pkg && typeof pkg.main === 'string') {
      const main = path().resolve(p, pkg.main);
      const found = asFile(main) || asFile(path().join(main, 'index'));
      if (found) return found;
    }
    return asFile(path().join(p, 'index'));
  }

  function fromNodeModules(dir, request) {
    const parts = request.split('/');
    const pkgName = request.startsWith('@') ? parts.slice(0, 2).join('/') : parts[0];
    const sub = '.' + request.slice(pkgName.length);
    for (let d = dir; ; d = path().dirname(d)) {
      if (path().basename(d) !== 'node_modules') {
        const root = path().join(d, 'node_modules', pkgName);
        if (isDir(root)) {
          const pkg = readJson(path().join(root, 'package.json'));
          if (pkg && pkg.exports !== undefined) {
            const t = exportTarget(pkg.exports, sub);
            if (t) return asFile(path().resolve(root, t));
            const e = new Error("Package subpath '" + sub + "' is not defined by \"exports\" in " + path().join(root, 'package.json'));
            e.code = 'ERR_PACKAGE_PATH_NOT_EXPORTED';
            throw e;
          }
          const target = path().join(root, sub);
          const found = sub === '.' ? asDirectory(root) : asFile(target) || (isDir(target) ? asDirectory(target) : null);
          if (found) return found;
        }
      }
      if (d === '/' || d === '.' || d === path().dirname(d)) return null;
    }
  }

  function resolve(request, dir) {
    request = String(request);
    const name = builtinName(request);
    if (name !== null) return 'node:' + name;
    let found;
    if (/^(\.{1,2}(\/|$)|\/)/.test(request)) {
      const p = path().resolve(dir, request);
      found = asFile(p) || (isDir(p) ? asDirectory(p) : null);
    } else {
      found = fromNodeModules(dir, request);
    }
    if (!found) {
      const e = new Error("Cannot find module '" + request + "'");
      e.code = 'MODULE_NOT_FOUND';
      throw e;
    }
    return found;
  }

  class Module {
    constructor(id = '', parent = null) {
      this.id = id;
      this.path = path().dirname(id);
      this.filename = id;
      this.exports = {};
      this.loaded = false;
      this.children = [];
      this.parent = parent;
      this.paths = [];
      for (let d = this.path; ; d = path().dirname(d)) {
        if (path().basename(d) !== 'node_modules') this.paths.push(path().join(d, 'node_modules'));
        if (d === '/' || d === path().dirname(d)) break;
      }
    }
    require(request) { return load(resolve(request, this.path), this); }
    _compile(source, filename) {
      const dirname = path().dirname(filename);
      const body = (source.startsWith('#!') ? '//' + source : source) + '\n//# sourceURL=' + filename;
      const wrapper = Function('exports', 'require', 'module', '__filename', '__dirname', body);
      wrapper.call(this.exports, this.exports, makeRequire(this), this, filename, dirname);
    }
  }
  Module._cache = Object.create(null);
  Module.builtinModules = [];
  Module.isBuiltin = (request) => builtinName(String(request)) !== null;
  Module.createRequire = (filename) => {
    const f = String(filename).startsWith('file:') ? decodeURIComponent(new URL(filename).pathname) : String(filename);
    return makeRequire(new Module(f.endsWith('/') ? path().join(f, '[eval]') : f));
  };
  Module.Module = Module;

  function load(filename, parent) {
    if (filename.startsWith('node:')) return native.builtin(filename.slice(5));
    const cached = Module._cache[filename];
    if (cached) {
      if (parent && !parent.children.includes(cached)) parent.children.push(cached);
      return cached.exports;
    }
    if (filename.endsWith('.mjs')) {
      const e = new Error('require() of ES Module ' + filename + ' is not supported; use import()');
      e.code = 'ERR_REQUIRE_ESM';
      throw e;
    }
    const module = new Module(filename, parent);
    Module._cache[filename] = module;
    if (parent) parent.children.push(module);
    try {
      if (filename.endsWith('.json')) {
        try { module.exports = JSON.parse(text(filename)); } catch (e) { e.message = filename + ': ' + e.message; throw e; }
      } else {
        module._compile(text(filename), filename);
      }
    } catch (e) {
      delete Module._cache[filename];
      throw e;
    }
    module.loaded = true;
    return module.exports;
  }

  function makeRequire(module) {
    const require = (request) => module.require(request);
    require.resolve = (request) => resolve(request, module.path);
    require.resolve.paths = (request) => (builtinName(String(request)) !== null ? null : module.paths.slice());
    require.cache = Module._cache;
    require.main = native.mainModule;
    return require;
  }

  /** Whether `file` is an ES module: `.mjs`, or `.js` / no extension under a package.json with `"type": "module"` */
  function isEsm(file) {
    if (file.endsWith('.mjs')) return true;
    if (!file.endsWith('.js') && path().extname(file) !== '') return false;
    for (let d = path().dirname(file); ; d = path().dirname(d)) {
      const pkg = readJson(path().join(d, 'package.json'));
      if (pkg) return pkg.type === 'module';
      if (d === '/' || d === path().dirname(d)) return false;
    }
  }

  /** The entry script: a CommonJS main module, or an imported ES module */
  native.runMain = (file) => {
    const filename = path().resolve(file);
    if (isEsm(filename)) return native.runModule(filename);
    const module = new Module(filename, null);
    module.id = '.';
    native.mainModule = module;
    Module._cache[filename] = module;
    module._compile(text(filename), filename);
    module.loaded = true;
    return undefined;
  };

  native.Module = Module;
  native.define('module', () => {
    Module.builtinModules = Object.keys(factories).filter((n) => !n.startsWith('_'));
    return Module;
  });
  native.makeRequire = makeRequire;
})(globalThis.__j8_native);
