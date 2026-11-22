'use strict';
// internalBinding('string_decoder') (src/string_decoder.cc): the constants; decoding is dune's string_decoder module
module.exports = {
  kIncompleteCharactersStart: 0,
  kIncompleteCharactersEnd: 4,
  kMissingBytes: 4,
  kBufferedBytes: 5,
  kEncodingField: 6,
  kNumFields: 7,
  kSize: 7,
  encodings: ['ascii', 'utf8', 'base64', 'utf16le', 'latin1', 'hex', 'buffer', 'base64url'],
};
