// jumpheap's prelude, on top of workerd's: `fetch` over the native HTTP client, `Matrix` (a JS matrix whose rows are
// Float64Arrays over public-heap memory) and `heaps` (this process's public heaps: make matrices, send them to other
// workers, receive theirs as `object` events).
(function (native) {
  'use strict';
  // Where a worker receives objects: `heaps.send` posts here, the request becomes an `object` event
  const OBJECT_PATH = '/__jumpheap/object';

  globalThis.fetch = async (input, init = {}) => {
    const req = input instanceof Request ? input : new Request(input, init);
    const [status, body] = await native.http.request(req.method, req.url, req._body == null ? '' : req._body);
    return new Response(body, { status });
  };

  // A matrix: an ordinary JS object in this isolate's heap. Its rows are Float64Arrays over memory in a public heap,
  // this process's or another one's; reading and writing them reads and writes that heap in place.
  class Matrix {
    constructor(memory) {
      this.path = memory.path;
      this.index = memory.index;
      this.data = memory.rows.map((b) => new Float64Array(b));
    }
    get rows() { return this.data.length; }
    get cols() { return this.data.length ? this.data[0].length : 0; }
    // Whether it lives in this process's heap
    get local() { return this.path === native.heaps.path; }
  }
  globalThis.Matrix = Matrix;

  const listeners = new Map();
  const heaps = {
    get path() { return native.heaps.path; },
    // A promise of a new matrix of `rows` × 100000 zeros in this process's heap
    async matrix(rows) { return new Matrix(await native.heaps.allocate(rows)); },
    // Sends `matrix` to the worker at `url` (its `object` listeners get it, as a Matrix over the same memory, with
    // `name` and `task`); gives what the receiver responded with
    async send(url, matrix, name, task = null) {
      const r = await fetch(url + OBJECT_PATH, { method: 'POST', body: JSON.stringify({ name, task, path: matrix.path, index: matrix.index }) });
      const body = await r.text();
      if (r.status !== 200) throw new Error(`${url} did not take ${name}: ${body}`);
      return JSON.parse(body);
    },
    addEventListener(type, f) {
      const list = listeners.get(type) || [];
      if (!list.includes(f)) list.push(f);
      listeners.set(type, list);
    },
    removeEventListener(type, f) {
      const list = listeners.get(type);
      if (list) listeners.set(type, list.filter((g) => g !== f));
    },
    // Collects this process's heap now
    collect() { native.heaps.collect(); },
  };
  Object.defineProperty(globalThis, 'heaps', { value: heaps, configurable: true });

  // A matrix another worker sent: taken from its heap, handed to the `object` listeners; the response is what they
  // passed to `respondWith` (null if nothing)
  async function receive(request) {
    const { name, task, path, index } = await request.json();
    const object = new Matrix(native.heaps.take(path, index));
    let reply = null;
    const event = { type: 'object', name, task, object, respondWith(v) { reply = v; } };
    for (const f of listeners.get('object') || []) f(event);
    return Response.json((await reply) ?? null);
  }

  // The worker's fetch, with OBJECT_PATH taken out in front of it
  const setup = __workerd.setup;
  __workerd.setup = function (ns, ...rest) {
    setup.call(this, ns, ...rest);
    const fetch = (req, env, ctx) => (new URL(req.url).pathname === OBJECT_PATH ? receive(req) : ns.default.fetch(req, env, ctx));
    this.worker = { default: { fetch } };
  };
})(globalThis.__j8_native);
