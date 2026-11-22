'use strict';
// internal/async_hooks without the engine's async context (see the public async_hooks stand-in): ids are handed out,
// hooks never fire
const { async_id_symbol, trigger_async_id_symbol, owner_symbol } = require('internal/binding/symbols');

let nextId = 2;
const noop = () => {};
const never = () => false;

module.exports = {
  executionAsyncId: () => 1,
  triggerAsyncId: () => 0,
  getHookArrays: () => [[], [], []],
  symbols: {
    async_id_symbol,
    trigger_async_id_symbol,
    init_symbol: Symbol('init'),
    before_symbol: Symbol('before'),
    after_symbol: Symbol('after'),
    destroy_symbol: Symbol('destroy'),
    promise_resolve_symbol: Symbol('promiseResolve'),
    owner_symbol,
  },
  constants: { kInit: 0, kBefore: 1, kAfter: 2, kDestroy: 3, kTotals: 4, kPromiseResolve: 5 },
  enableHooks: noop,
  disableHooks: noop,
  updatePromiseHookMode: noop,
  clearDefaultTriggerAsyncId: noop,
  clearAsyncIdStack: noop,
  hasAsyncIdStack: never,
  executionAsyncResource: () => ({}),
  newAsyncId: () => nextId++,
  getOrSetAsyncId(object) {
    if (Object.hasOwn(object, async_id_symbol)) return object[async_id_symbol];
    return (object[async_id_symbol] = nextId++);
  },
  getDefaultTriggerAsyncId: () => 1,
  defaultTriggerAsyncIdScope: (triggerAsyncId, block, ...args) => block(...args),
  enabledHooksExist: never,
  initHooksExist: never,
  afterHooksExist: never,
  destroyHooksExist: never,
  emitInit: noop,
  emitBefore: noop,
  emitAfter: noop,
  emitDestroy: noop,
  pushAsyncContext: noop,
  popAsyncContext: never,
  registerDestroyHook: noop,
  useDomainTrampoline: noop,
  nativeHooks: { init: noop, before: noop, after: noop, destroy: noop, promise_resolve: noop },
};
