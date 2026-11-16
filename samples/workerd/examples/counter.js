// The simplest Durable Object: one counter per name, with state kept in storage.
export class Counter {
  constructor(state, env) {
    this.state = state;
    this.env = env;
  }

  async fetch(request) {
    const url = new URL(request.url);
    let n = (await this.state.storage.get('n')) || 0;
    if (url.pathname.endsWith('/inc')) {
      n += 1;
      await this.state.storage.put('n', n);
    }
    return new Response(JSON.stringify({ id: this.state.id.toString(), count: n }), {
      headers: { 'content-type': 'application/json' },
    });
  }
}

export default {
  async fetch(request, env) {
    const url = new URL(request.url);
    const name = url.searchParams.get('name') || 'default';
    if (url.pathname === '/') {
      return new Response('hello from j8 workerd\n');
    }
    const stub = env.COUNTER.get(env.COUNTER.idFromName(name));
    return stub.fetch(request);
  },
};
