'use strict';
// internalBinding('config') (src/node_config.cc): what this build of dune has
module.exports = {
  isDebugBuild: false,
  openSSLIsBoringSSL: false,
  hasOpenSSL: false,
  fipsMode: false,
  hasIntl: false,
  hasTracing: false,
  hasNodeOptions: false,
  hasInspector: false,
  noBrowserGlobals: false,
  bits: 64,
};
