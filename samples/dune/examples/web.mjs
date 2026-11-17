// A tour of the Web layer: `dune --web examples/web.mjs` (also runs under the Node layer)
const url = new URL('/search?q=j8&lang=rust', 'http://example.com');
console.log(url.href, Object.fromEntries(url.searchParams));
console.log(new TextDecoder().decode(new TextEncoder().encode('ünïcode')), btoa('j8'), crypto.randomUUID());

const controller = new AbortController();
controller.signal.addEventListener('abort', () => console.log('aborted:', controller.signal.reason.name));
setTimeout(() => controller.abort(), 10);

console.log(structuredClone({ when: new Date(0), tags: new Set(['a', 'b']) }));
fetch('data:application/json,{"from":"a data URL"}')
  .then((r) => r.json())
  .then((body) => console.log('fetched', body));
