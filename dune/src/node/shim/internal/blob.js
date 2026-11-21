'use strict';
// internal/blob: the Web layer's Blob
const { Blob } = globalThis;
module.exports = {
  Blob,
  isBlob: (value) => value instanceof Blob,
  resolveObjectURL: () => undefined,
};
