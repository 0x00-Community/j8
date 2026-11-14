// dune's prelude: runs once before user code. CommonJS require / module, built-in modules fs / path / process.
(function (native) {
  'use strict';
  const fs = native.fs;
  const path = native.path;
  const builtins = { fs: fs, path: path, process: globalThis.process };
  const cache = Object.create(null);

  function resolveFile(file) {
    if (fs.existsSync(file) && !fs.isDirectory(file)) return file;
    for (const ext of ['.js', '.json', '.cjs']) {
      if (fs.existsSync(file + ext)) return file + ext;
    }
    if (fs.isDirectory(file)) {
      const pkg = path.join(file, 'package.json');
      if (fs.existsSync(pkg)) {
        const main = JSON.parse(fs.readFileSync(pkg)).main;
        if (main) return resolveFile(path.join(file, main));
      }
      if (fs.existsSync(path.join(file, 'index.js'))) return path.join(file, 'index.js');
    }
    return null;
  }

  function resolve(from, request) {
    const name = request.startsWith('node:') ? request.slice(5) : request;
    if (Object.prototype.hasOwnProperty.call(builtins, name)) return 'node:' + name;
    let found = null;
    if (request.startsWith('./') || request.startsWith('../') || request.startsWith('/')) {
      found = resolveFile(path.resolve(from, request));
    } else {
      // node_modules: search upward from the current directory
      let dir = from;
      for (;;) {
        found = resolveFile(path.join(dir, 'node_modules', request));
        if (found || dir === '/' || dir === '.') break;
        dir = path.dirname(dir);
      }
    }
    if (!found) {
      const e = new Error("Cannot find module '" + request + "'");
      e.code = 'MODULE_NOT_FOUND';
      throw e;
    }
    return found;
  }

  function load(filename) {
    if (filename.startsWith('node:')) return builtins[filename.slice(5)];
    if (cache[filename]) return cache[filename].exports;
    const module = { id: filename, filename: filename, exports: {}, loaded: false };
    cache[filename] = module;
    const source = fs.readFileSync(filename);
    if (filename.endsWith('.json')) {
      module.exports = JSON.parse(source);
    } else {
      const dirname = path.dirname(filename);
      const wrapper = Function('exports', 'require', 'module', '__filename', '__dirname', source);
      wrapper.call(module.exports, module.exports, makeRequire(dirname), module, filename, dirname);
    }
    module.loaded = true;
    return module.exports;
  }

  function makeRequire(dir) {
    const require = function (request) { return load(resolve(dir, String(request))); };
    require.resolve = function (request) { return resolve(dir, String(request)); };
    require.cache = cache;
    return require;
  }

  globalThis.require = makeRequire(process.cwd());
  process.nextTick = function (f, ...args) { Promise.resolve().then(() => f(...args)); };
  Object.defineProperty(globalThis, 'global', { value: globalThis, writable: true, configurable: true });
  // Run the entry script, wrapped as a CommonJS module
  native.runMain = function (file) { return load(path.resolve(process.cwd(), file)); };
})(globalThis.__dune);
