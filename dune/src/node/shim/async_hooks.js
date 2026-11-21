'use strict';
// async_hooks without the engine's async context: hooks never fire and AsyncLocalStorage only reaches code called
// synchronously inside run() (not across await, timers or callbacks yet)
const { validateFunction, validateString } = require('internal/validators');

let nextId = 2;
let currentStores = new Map();

class AsyncResource {
  #asyncId;
  #triggerAsyncId;
  #stores;

  constructor(type, options = {}) {
    validateString(type, 'type');
    const trigger = typeof options === 'number' ? options : options?.triggerAsyncId;
    this.#asyncId = nextId++;
    this.#triggerAsyncId = trigger ?? 1;
    this.#stores = currentStores;
  }

  runInAsyncScope(fn, thisArg, ...args) {
    const saved = currentStores;
    currentStores = this.#stores;
    try {
      return Reflect.apply(fn, thisArg, args);
    } finally {
      currentStores = saved;
    }
  }

  emitDestroy() { return this; }
  asyncId() { return this.#asyncId; }
  triggerAsyncId() { return this.#triggerAsyncId; }

  bind(fn, thisArg) {
    validateFunction(fn, 'fn');
    const resource = this;
    const bound = function(...args) { return resource.runInAsyncScope(fn, thisArg === undefined ? this : thisArg, ...args); };
    Object.defineProperties(bound, {
      length: { __proto__: null, configurable: true, enumerable: false, value: fn.length, writable: false },
      asyncResource: { __proto__: null, configurable: true, enumerable: true, value: this, writable: true },
    });
    return bound;
  }

  static bind(fn, type, thisArg) {
    return new AsyncResource(type || fn.name || 'bound-anonymous-fn').bind(fn, thisArg);
  }
}

class AsyncLocalStorage {
  #defaultValue;
  #name;

  constructor(options = {}) {
    this.#defaultValue = options.defaultValue;
    this.#name = options.name ?? '';
  }

  get name() { return this.#name; }

  static bind(fn) { return AsyncResource.bind(fn, 'AsyncLocalStorage'); }

  static snapshot() {
    const resource = new AsyncResource('AsyncLocalStorageSnapshot');
    return (fn, ...args) => resource.runInAsyncScope(fn, undefined, ...args);
  }

  disable() {
    currentStores = new Map(currentStores);
    currentStores.delete(this);
  }

  getStore() {
    return currentStores.has(this) ? currentStores.get(this) : this.#defaultValue;
  }

  enterWith(store) {
    currentStores = new Map(currentStores);
    currentStores.set(this, store);
  }

  run(store, fn, ...args) {
    const saved = currentStores;
    currentStores = new Map(currentStores);
    currentStores.set(this, store);
    try {
      return Reflect.apply(fn, null, args);
    } finally {
      currentStores = saved;
    }
  }

  exit(fn, ...args) {
    const saved = currentStores;
    currentStores = new Map(currentStores);
    currentStores.delete(this);
    try {
      return Reflect.apply(fn, null, args);
    } finally {
      currentStores = saved;
    }
  }
}

class AsyncHook {
  enable() { return this; }
  disable() { return this; }
}

module.exports = {
  AsyncLocalStorage,
  createHook: () => new AsyncHook(),
  executionAsyncId: () => 1,
  triggerAsyncId: () => 0,
  executionAsyncResource: () => ({}),
  asyncWrapProviders: Object.freeze({ __proto__: null }),
  AsyncResource,
};
