// Node's own library: the files of Node v25.9.0's lib/ (src/node/lib) and dune's stand-ins for the parts that are C++
// in Node (src/node/shim, which win over lib), compiled the way Node compiles its built-in modules, with `primordials`
// and `internalBinding`. Every public module found there replaces dune's hand-written one of the same name (their `define` is ignored).
(function (native) {
  'use strict';
  const { lib } = native;
  const cache = Object.create(null);
  const BINDING = 'internal/binding/';
  const PARAMS = ['exports', 'require', 'module', 'process', 'internalBinding', 'primordials'];
  const SETUP = {
    __proto__: null,
    'internal/util/debuglog': (m) => m.initializeDebugEnv(globalThis.process.env.NODE_DEBUG),
  };

  if (typeof Error.captureStackTrace !== 'function') {
    Object.defineProperty(Error, 'captureStackTrace', {
      value: function captureStackTrace(target) {
        if (target === null || (typeof target !== 'object' && typeof target !== 'function')) {
          throw new TypeError('The "targetObject" argument must be of type object');
        }
        let text;
        try { text = Error.prototype.toString.call(target); } catch { text = 'Error'; }
        Object.defineProperty(target, 'stack', { value: text, writable: true, configurable: true, enumerable: false });
      },
      writable: true,
      configurable: true,
    });
  }
  if (!('stackTraceLimit' in Error)) Error.stackTraceLimit = 10;
  for (const name of ['dispose', 'asyncDispose']) {
    if (typeof Symbol[name] !== 'symbol') {
      Object.defineProperty(Symbol, name, { value: Symbol('Symbol.' + name), writable: false, enumerable: false, configurable: false });
    }
  }

  let primordials;
  const getPrimordials = () => {
    if (primordials === undefined) {
      primordials = {};
      Function('primordials', lib.source('internal/per_context/primordials'))(primordials);
    }
    return primordials;
  };

  function internalBinding(name) {
    if (lib.source(BINDING + name) === undefined) throw new Error('No such binding: ' + name);
    return requireLib(BINDING + name);
  }

  function requireLib(id) {
    if (id.startsWith('node:')) id = id.slice(5);
    const cached = cache[id];
    if (cached !== undefined) return cached.exports;
    const source = lib.source(id);
    if (source === undefined) return native.builtin(id);
    const module = { exports: {}, id, loaded: false };
    cache[id] = module;
    try {
      Function(...PARAMS, source)(module.exports, requireLib, module, globalThis.process, internalBinding, getPrimordials());
    } catch (e) {
      delete cache[id];
      throw e;
    }
    module.loaded = true;
    if (id in SETUP) SETUP[id](module.exports);
    return module.exports;
  }

  native.requireLib = requireLib;
  const define = native.define;
  native.define = (name, factory) => {
    if (lib.source(name) === undefined) define(name, factory);
  };
  for (const id of lib.ids()) {
    if (!id.startsWith('internal/')) define(id, () => requireLib(id));
  }
})(globalThis.__j8_native);
