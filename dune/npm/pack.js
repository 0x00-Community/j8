#!/usr/bin/env node
// Lays out the npm packages of one dune release, ready for `npm publish` in each directory:
//   <out>/dune/            the main package (@j8dune/dune): placeholders, install.js, optionalDependencies on all below
//   <out>/<platform>/      one per platforms.json entry found in <dist> (@j8dune/dune-linux-x64, @j8dune/dune-darwin-arm64, ...)
// <dist>/<rust target>/dune[.exe] are the release binaries (cargo build --release --target <rust target>); the
// version is the one in ../Cargo.toml. Publish the platform packages before the main package.
//   node npm/pack.js <dist> <out>
'use strict';
const fs = require('fs');
const path = require('path');

const SCOPE = '@j8dune';
const MAIN = `${SCOPE}/dune`;
const HERE = __dirname;
const REPOSITORY = { type: 'git', url: 'git+https://github.com/0x00-Community/j8.git', directory: 'dune' };

const [dist, out] = process.argv.slice(2);
if (!dist || !out) {
  console.error('usage: node npm/pack.js <dist> <out>');
  process.exit(2);
}
const cargo = fs.readFileSync(path.join(HERE, '..', 'Cargo.toml'), 'utf8');
const version = /^version\s*=\s*"([^"]+)"/m.exec(cargo)[1];
const platforms = require('./platforms.json').filter((p) =>
  fs.existsSync(path.join(dist, p.target, `dune${p.exe || ''}`)),
);
if (platforms.length === 0) {
  console.error(`${dist}: no binaries (expected <dist>/<rust target>/dune)`);
  process.exit(1);
}
const common = { version, license: 'MIT', homepage: 'https://j8.app238.com/dune.html', repository: REPOSITORY };

function write(file, content) {
  fs.mkdirSync(path.dirname(file), { recursive: true });
  fs.writeFileSync(file, typeof content === 'string' ? content : `${JSON.stringify(content, null, 2)}\n`);
}

for (const p of platforms) {
  const dir = path.join(out, p.name);
  const exe = `dune${p.exe || ''}`;
  write(path.join(dir, 'package.json'), {
    name: `${SCOPE}/dune-${p.name}`,
    description: `The dune binary for ${p.name} (installed by ${MAIN})`,
    ...common,
    preferUnplugged: true,
    os: [p.os],
    cpu: [p.cpu],
    ...(p.libc && { libc: [p.libc] }),
  });
  write(path.join(dir, 'README.md'), `# ${SCOPE}/dune-${p.name}\n\nThe \`dune\` binary for ${p.name}. Install [${MAIN}](https://www.npmjs.com/package/${MAIN}) instead.\n`);
  fs.mkdirSync(path.join(dir, 'bin'), { recursive: true });
  fs.copyFileSync(path.join(dist, p.target, exe), path.join(dir, 'bin', exe));
  fs.chmodSync(path.join(dir, 'bin', exe), 0o755);
}

const main = path.join(out, 'dune');
fs.cpSync(path.join(HERE, 'dune'), main, { recursive: true });
fs.copyFileSync(path.join(HERE, 'platforms.json'), path.join(main, 'platforms.json'));
write(path.join(main, 'package.json'), {
  name: MAIN,
  description: 'dune and dunx: a JavaScript runtime on j8 (Node-compatible and Web-standard layers) and a runner for npm packages\' executables',
  ...common,
  keywords: ['javascript', 'runtime', 'node', 'npx', 'j8'],
  bin: { dune: 'bin/dune.exe', dunx: 'bin/dunx.exe' },
  scripts: { postinstall: 'node install.js' },
  engines: { node: '>=18' },
  os: [...new Set(platforms.map((p) => p.os))],
  cpu: [...new Set(platforms.map((p) => p.cpu))],
  optionalDependencies: Object.fromEntries(platforms.map((p) => [`${SCOPE}/dune-${p.name}`, version])),
});
console.log(`${MAIN}@${version}: ${platforms.map((p) => p.name).join(', ')}`);
