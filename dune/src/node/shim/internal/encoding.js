'use strict';
// internal/encoding: the Web layer's TextDecoder / TextEncoder
const LABELS = { 'utf-8': 'utf-8', utf8: 'utf-8', 'unicode-1-1-utf-8': 'utf-8', 'utf-16le': 'utf-16le', 'utf-16': 'utf-16le', latin1: 'windows-1252', 'iso-8859-1': 'windows-1252', ascii: 'windows-1252' };
module.exports = {
  getEncodingFromLabel: (label) => LABELS[String(label).trim().toLowerCase()],
  TextDecoder: globalThis.TextDecoder,
  TextEncoder: globalThis.TextEncoder,
};
