// One resident worker: it owns one matrix (a JS object over its own public heap), sends it to other workers, and
// listens for theirs. What arrives in an `object` event is a JS Matrix in this isolate whose rows live in the
// sender's heap; everything below computes on such objects in plain JS.
//
//   POST /create?rows=&seed=        make this worker's matrix and fill it: m[i][j] = (i·7 + j·13 + seed·31) mod 9 − 4
//   POST /send?to=&name=&task=      send it to the worker at `to`; gives what that worker responded
//   GET  /sum?of=                   sum of this worker's matrix (`self`) or of one it received
//
// Tasks a received matrix comes with:
//   overwrite   write this worker's first half of rows over it (into the sender's heap); gives the rows written
//   dot         dot product (as long vectors) of this worker's matrix with it; gives { dot }
//   (none)      keep it
let mine = null;
const received = new Map();
const json = (value, status = 200) => Response.json(value, { status });

// One row's values for row i: a period of 9 along j, so a row is one base pattern shifted (13·s ≡ k (mod 9) ⇔ s = 7k
// mod 9); `set` copies it in
function fill(m, seed) {
  const base = new Float64Array(m.cols + 9);
  for (let j = 0; j < base.length; j++) base[j] = ((j * 13) % 9) - 4;
  for (let i = 0; i < m.rows; i++) {
    const k = (i * 7 + seed * 31) % 9;
    const s = (7 * k) % 9;
    m.data[i].set(base.subarray(s, s + m.cols));
  }
}

function sum(m) {
  let s = 0;
  for (let i = 0; i < m.rows; i++) {
    const row = m.data[i];
    for (let j = 0; j < row.length; j++) s += row[j];
  }
  return s;
}

function dot(x, y) {
  const rows = Math.min(x.rows, y.rows);
  let s = 0;
  for (let i = 0; i < rows; i++) {
    const a = x.data[i];
    const b = y.data[i];
    for (let j = 0; j < a.length; j++) s += a[j] * b[j];
  }
  return s;
}

// The first `n` rows of `from` written over `to`'s
function overwrite(from, to, n) {
  for (let i = 0; i < n; i++) to.data[i].set(from.data[i]);
  return n;
}

heaps.addEventListener('object', (e) => {
  received.set(e.name, e.object);
  switch (e.task) {
    case 'overwrite':
      e.respondWith({ rows: overwrite(mine, e.object, Math.min(mine.rows, e.object.rows) >> 1) });
      break;
    case 'dot':
      e.respondWith({ dot: dot(mine, e.object) });
      break;
    default:
      e.respondWith({ kept: e.name, rows: e.object.rows, local: e.object.local });
  }
});

export default {
  async fetch(request) {
    const url = new URL(request.url);
    const q = url.searchParams;
    try {
      switch (url.pathname) {
        case '/create':
          mine = await heaps.matrix(Number(q.get('rows')));
          fill(mine, Number(q.get('seed')));
          return json({ rows: mine.rows, cols: mine.cols, heap: heaps.path });
        case '/send':
          return json(await heaps.send(q.get('to'), mine, q.get('name'), q.get('task')));
        case '/sum': {
          const m = q.get('of') === 'self' ? mine : received.get(q.get('of'));
          if (!m) return json({ error: 'no matrix ' + q.get('of') }, 404);
          return json({ sum: sum(m) });
        }
        default:
          return json({ error: 'not found' }, 404);
      }
    } catch (e) {
      return json({ error: String((e && e.message) || e) }, 500);
    }
  },
};
