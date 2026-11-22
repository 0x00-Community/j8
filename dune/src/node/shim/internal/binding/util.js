'use strict';
// internalBinding('util') (src/node_util.cc), on dune's v8 natives
const { v8, os } = globalThis.__j8_native;

const constants = {
  kPending: 0, kFulfilled: 1, kRejected: 2,
  kExiting: 0, kExitCode: 1, kHasExitCode: 2,
  ALL_PROPERTIES: 0, ONLY_WRITABLE: 1, ONLY_ENUMERABLE: 2, ONLY_CONFIGURABLE: 4, SKIP_STRINGS: 8, SKIP_SYMBOLS: 16,
  kDisallowCloneAndTransfer: 0, kTransferable: 1, kCloneable: 2,
};

const PRIVATE = [
  ['arrow_message_private_symbol', 'node:arrowMessage'],
  ['contextify_context_private_symbol', 'node:contextify:context'],
  ['decorated_private_symbol', 'node:decorated'],
  ['empty_context_frame_sentinel_symbol', 'node:empty_context_frame_sentinel'],
  ['transfer_mode_private_symbol', 'node:transfer_mode'],
  ['host_defined_option_symbol', 'node:host_defined_option_symbol'],
  ['js_transferable_wrapper_private_symbol', 'node:js_transferable_wrapper'],
  ['entry_point_module_private_symbol', 'node:entry_point_module'],
  ['entry_point_promise_private_symbol', 'node:entry_point_promise'],
  ['module_source_private_symbol', 'node:module_source'],
  ['module_export_names_private_symbol', 'node:module_export_names'],
  ['module_circular_visited_private_symbol', 'node:module_circular_visited'],
  ['module_export_private_symbol', 'node:module_export'],
  ['module_first_parent_private_symbol', 'node:module_first_parent'],
  ['module_last_parent_private_symbol', 'node:module_last_parent'],
  ['napi_type_tag', 'node:napi:type_tag'],
  ['napi_wrapper', 'node:napi:wrapper'],
  ['untransferable_object_private_symbol', 'node:untransferableObject'],
  ['exit_info_private_symbol', 'node:exit_info_private_symbol'],
  ['promise_trace_id', 'node:promise_trace_id'],
  ['source_map_data_private_symbol', 'node:source_map_data_private_symbol'],
];
const privateSymbols = Object.fromEntries(PRIVATE.map(([name, description]) => [name, Symbol(description)]));

const isIndex = (key) => typeof key === 'string' && key !== '' && String(Number(key) >>> 0) === key && Number(key) !== 4294967295;

function getOwnNonIndexProperties(object, filter) {
  return Reflect.ownKeys(object).filter((key) => {
    if (isIndex(key)) return false;
    if (typeof key === 'symbol' ? filter & constants.SKIP_SYMBOLS : filter & constants.SKIP_STRINGS) return false;
    if (filter & (constants.ONLY_ENUMERABLE | constants.ONLY_WRITABLE | constants.ONLY_CONFIGURABLE)) {
      const d = Reflect.getOwnPropertyDescriptor(object, key);
      if (d === undefined) return false;
      if (filter & constants.ONLY_ENUMERABLE && !d.enumerable) return false;
      if (filter & constants.ONLY_WRITABLE && 'value' in d && !d.writable) return false;
      if (filter & constants.ONLY_CONFIGURABLE && !d.configurable) return false;
    }
    return true;
  });
}

function getConstructorName(object) {
  for (let o = object; o !== null && o !== undefined; o = Reflect.getPrototypeOf(o)) {
    const d = Reflect.getOwnPropertyDescriptor(o, 'constructor');
    if (d !== undefined && typeof d.value === 'function' && typeof d.value.name === 'string' && d.value.name !== '') return d.value.name;
  }
  return 'Object';
}

function defineLazyProperties(target, id, keys, enumerable = true) {
  const require = globalThis.__j8_native.requireLib;
  for (const key of keys) {
    let value;
    let loaded = false;
    Reflect.defineProperty(target, key, {
      __proto__: null,
      enumerable,
      configurable: true,
      get() {
        if (!loaded) { value = require(id)[key]; loaded = true; }
        return value;
      },
      set(v) {
        Reflect.defineProperty(this, key, { __proto__: null, value: v, writable: true, enumerable, configurable: true });
      },
    });
  }
}

function previewEntries(object, isIterator) {
  const all = v8.entries(object);
  if (all === undefined) return undefined;
  const [isKeyValue, ...entries] = all;
  return isIterator === undefined ? entries : [entries, isKeyValue];
}

function getProxyDetails(value, showProxy) {
  const parts = v8.proxy(value);
  if (parts === undefined) return undefined;
  return showProxy === undefined || showProxy === true ? parts : parts[0];
}

module.exports = {
  constants,
  privateSymbols,
  shouldAbortOnUncaughtToggle: new Int32Array(1),
  getOwnNonIndexProperties,
  getConstructorName,
  defineLazyProperties,
  previewEntries,
  getProxyDetails,
  getPromiseDetails: (value) => v8.promise(value),
  getExternalValue: () => 0n,
  getCallerLocation: () => undefined,
  getCallSites: () => [],
  isInsideNodeModules: () => false,
  arrayBufferViewHasBuffer: () => true,
  guessHandleType: (fd) => os.handleType(fd),
  sleep: (ms) => { const end = Date.now() + ms; while (Date.now() < end); },
  constructSharedArrayBuffer: (length) => new SharedArrayBuffer(length),
  parseEnv(content) {
    const out = {};
    for (const line of String(content).split(/\r?\n/)) {
      const m = /^\s*(?:export\s+)?([\w.-]+)\s*=\s*(.*)?\s*$/.exec(line);
      if (!m) continue;
      let value = (m[2] || '').trim();
      const q = value[0];
      if ((q === '"' || q === "'" || q === '`') && value.endsWith(q) && value.length > 1) {
        value = value.slice(1, -1);
        if (q === '"') value = value.replace(/\\n/g, '\n');
      } else {
        value = value.replace(/\s+#.*$/, '');
      }
      out[m[1]] = value;
    }
    return out;
  },
};
