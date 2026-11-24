// Which @litany/dune-<platform> package holds the binary for this machine, and where in it the binary is.
'use strict';
const path = require('path');
const platforms = require('./platforms.json');

const SCOPE = '@litany';

/** `glibc` or `musl` on Linux (Node built against glibc reports its version), nothing elsewhere */
function libc() {
  if (process.platform !== 'linux') return undefined;
  const header = process.report && process.report.getReport().header;
  return header && header.glibcVersionRuntime ? 'glibc' : 'musl';
}

const host = platforms.find(
  (p) => p.os === process.platform && p.cpu === process.arch && (p.libc === undefined || p.libc === libc()),
);

module.exports = host && {
  ...host,
  package: `${SCOPE}/dune-${host.name}`,
  /** The binary's path inside the platform package */
  file: path.posix.join('bin', `dune${host.exe || ''}`),
};
