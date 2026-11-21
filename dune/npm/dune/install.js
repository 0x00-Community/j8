// postinstall: put the platform package's dune binary at bin/dune.exe (replacing the Node placeholder) and link
// bin/dunx.exe to it. The platform package comes from optionalDependencies; when the package manager left it out, it
// is installed with npm, and failing that downloaded from the registry. On any failure the placeholders stay, and
// they still work while the platform package is there.
'use strict';
const fs = require('fs');
const os = require('os');
const path = require('path');
const zlib = require('zlib');
const { spawnSync } = require('child_process');
const host = require('./platform.js');
const { version } = require('./package.json');

const BIN = path.join(__dirname, 'bin');
const REGISTRY = (process.env.npm_config_registry || 'https://registry.npmjs.org/').replace(/\/?$/, '/');
/** Size of a tar header and of the blocks entries are padded to */
const TAR_BLOCK = 512;
const EXECUTABLE = 0o755;

/** The binary when it runs (`dune --version` succeeds) */
function check(exe) {
  return spawnSync(exe, ['--version'], { stdio: 'ignore' }).status === 0 ? exe : undefined;
}

function resolved() {
  try {
    return check(require.resolve(`${host.package}/${host.file}`));
  } catch {
    return undefined;
  }
}

/** `npm install` the platform package into a scratch directory */
function npmInstall(dir) {
  fs.writeFileSync(path.join(dir, 'package.json'), '{}');
  const npm = process.platform === 'win32' ? 'npm.cmd' : 'npm';
  const args = ['install', '--no-save', '--no-audit', '--no-fund', '--loglevel=error', `${host.package}@${version}`];
  const env = { ...process.env, npm_config_global: undefined };
  spawnSync(npm, args, { cwd: dir, stdio: 'ignore', env, shell: process.platform === 'win32' });
  return check(path.join(dir, 'node_modules', host.package, host.file));
}

/** Download the platform package's tarball and take the binary out of it */
async function download(dir) {
  const base = host.package.split('/')[1];
  const response = await fetch(`${REGISTRY}${host.package}/-/${base}-${version}.tgz`);
  if (!response.ok) throw new Error(`${response.status} ${response.statusText}`);
  const tar = zlib.gunzipSync(Buffer.from(await response.arrayBuffer()));
  const field = (at, length) => tar.toString('latin1', at, at + length).replace(/\0.*$/s, '');
  for (let at = 0; at + TAR_BLOCK <= tar.length; ) {
    const name = field(at, 100);
    const size = parseInt(field(at + 124, 12).trim() || '0', 8);
    at += TAR_BLOCK;
    if (name === `package/${host.file}`) {
      const exe = path.join(dir, path.basename(host.file));
      fs.writeFileSync(exe, tar.subarray(at, at + size), { mode: EXECUTABLE });
      return check(exe);
    }
    at += Math.ceil(size / TAR_BLOCK) * TAR_BLOCK;
  }
  return undefined;
}

/** Replace `to` with `from`: a hard link when the file system allows, else a copy */
function place(from, to) {
  fs.rmSync(to, { force: true });
  try {
    fs.linkSync(from, to);
  } catch {
    fs.copyFileSync(from, to);
  }
  fs.chmodSync(to, EXECUTABLE);
}

async function main() {
  if (!host) {
    console.warn(`dune: no prebuilt binary for ${process.platform}-${process.arch}; build it with cargo install j8dune`);
    return;
  }
  const scratch = fs.mkdtempSync(path.join(os.tmpdir(), 'dune-'));
  try {
    const exe = resolved() || npmInstall(scratch) || (await download(scratch).catch(() => undefined));
    if (!exe) throw new Error(`could not get ${host.package}@${version}`);
    const dune = path.join(BIN, 'dune.exe');
    place(exe, dune);
    place(dune, path.join(BIN, 'dunx.exe'));
  } finally {
    fs.rmSync(scratch, { recursive: true, force: true });
  }
}

main().catch((e) => console.warn(`dune: postinstall: ${e.message}; dune and dunx will start through Node`));
