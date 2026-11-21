'use strict';
// internalBinding('types') (src/node_types.cc), on dune's v8 natives
const { v8 } = globalThis.__j8_native;
const kind = (value) => v8.kind(value);
const is = (name) => (value) => kind(value) === name;
const { getPrototypeOf } = Reflect;
const AsyncFunction = getPrototypeOf(async function () {});
const GeneratorFunction = getPrototypeOf(function* () {});
const AsyncGeneratorFunction = getPrototypeOf(async function* () {});
const BOXED = ['NumberObject', 'StringObject', 'BooleanObject', 'BigIntObject', 'SymbolObject'];

module.exports = {
  isExternal: is('External'),
  isDate: is('Date'),
  isArgumentsObject: is('Arguments'),
  isBigIntObject: is('BigIntObject'),
  isBooleanObject: is('BooleanObject'),
  isNumberObject: is('NumberObject'),
  isStringObject: is('StringObject'),
  isSymbolObject: is('SymbolObject'),
  isBoxedPrimitive: (value) => BOXED.includes(kind(value)),
  isNativeError: is('Error'),
  isRegExp: is('RegExp'),
  isAsyncFunction: (value) => typeof value === 'function' && (getPrototypeOf(value) === AsyncFunction || getPrototypeOf(value) === AsyncGeneratorFunction),
  isGeneratorFunction: (value) => typeof value === 'function' && (getPrototypeOf(value) === GeneratorFunction || getPrototypeOf(value) === AsyncGeneratorFunction),
  isGeneratorObject: is('Generator'),
  isPromise: is('Promise'),
  isMap: is('Map'),
  isSet: is('Set'),
  isMapIterator: is('MapIterator'),
  isSetIterator: is('SetIterator'),
  isWeakMap: is('WeakMap'),
  isWeakSet: is('WeakSet'),
  isArrayBuffer: is('ArrayBuffer'),
  isDataView: is('DataView'),
  isSharedArrayBuffer: (value) => typeof SharedArrayBuffer === 'function' && value instanceof SharedArrayBuffer,
  isAnyArrayBuffer: (value) => kind(value) === 'ArrayBuffer' || (typeof SharedArrayBuffer === 'function' && value instanceof SharedArrayBuffer),
  isProxy: is('Proxy'),
  isModuleNamespaceObject: is('ModuleNamespace'),
};
