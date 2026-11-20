// A tour of the Node layer: `dune examples/node.js`
const fs = require('node:fs');
const path = require('node:path');
const os = require('node:os');
const { EventEmitter, once } = require('node:events');
const { setTimeout: sleep } = require('node:timers/promises');

const dir = fs.mkdtempSync(path.join(os.tmpdir(), 'dune-'));
fs.writeFileSync(path.join(dir, 'hello.txt'), 'héllo');
console.log(fs.readFileSync(path.join(dir, 'hello.txt')), fs.readFileSync(path.join(dir, 'hello.txt'), 'utf8'));
console.log(Buffer.from('dune').toString('base64'), path.parse('/srv/app/index.js'));

const bus = new EventEmitter();
(async () => {
  setTimeout(() => bus.emit('ready', 42), 10);
  const [value] = await once(bus, 'ready');
  console.log('ready', value, await sleep(5, 'after a nap'));
  console.log(await fs.promises.readdir(dir));
  fs.rmSync(dir, { recursive: true });
  process.exitCode = 0;
})();
process.on('exit', (code) => console.log('exit', code));
