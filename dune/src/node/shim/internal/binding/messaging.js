'use strict';
// internalBinding('messaging') (src/node_messaging.cc): DOMException and structuredClone are the Web layer's; no
// MessagePort yet
module.exports = {
  DOMException: globalThis.DOMException,
  structuredClone: (value, options) => globalThis.structuredClone(value, options),
  setDeserializerCreateObjectFunction() {},
};
