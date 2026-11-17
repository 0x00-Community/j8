// console: Node-style value formatting (`inspect`, `format`), shared with node:util through __j8_native.
(function (native) {
  'use strict';
  const { stdio, clock } = native;
  const typedArrayProto = Object.getPrototypeOf(Uint8Array.prototype);
  const typedArrayTag = Object.getOwnPropertyDescriptor(typedArrayProto, Symbol.toStringTag).get;
  const identifier = /^[A-Za-z_$][\w$]*$/;
  const custom = Symbol.for('nodejs.util.inspect.custom');

  function quote(s) {
    const q = s.includes("'") && !s.includes('"') ? '"' : "'";
    return q + s.replace(/\\/g, '\\\\').replace(/\n/g, '\\n').replace(new RegExp(q, 'g'), '\\' + q) + q;
  }

  function key(k) {
    if (typeof k === 'symbol') return '[' + k.toString() + ']';
    return identifier.test(k) ? k : quote(k);
  }

  function constructorName(o) {
    for (let p = o; p !== null; p = Object.getPrototypeOf(p)) {
      const d = Object.getOwnPropertyDescriptor(p, 'constructor');
      if (d && typeof d.value === 'function' && d.value.name) return d.value.name;
    }
    return null;
  }

  function functionText(f) {
    const src = Function.prototype.toString.call(f);
    const isClass = /^class\b/.test(src);
    const kind = isClass ? 'class' : /^async\s/.test(src) ? 'AsyncFunction' : /^(async\s*)?function\s*\*/.test(src) ? 'GeneratorFunction' : 'Function';
    if (isClass) {
      const base = Object.getPrototypeOf(f);
      const ext = base && base !== Function.prototype && base.name ? ' extends ' + base.name : '';
      return '[class ' + (f.name || '(anonymous)') + ext + ']';
    }
    return '[' + kind + (f.name ? ': ' + f.name : ' (anonymous)') + ']';
  }

  function errorText(e) {
    if (typeof e.stack === 'string' && e.stack) return e.stack;
    let name;
    let message;
    try { name = String(e.name); message = String(e.message); } catch { return '[Error]'; }
    return message ? name + ': ' + message : name;
  }

  /** Joins entries on one line if they fit, otherwise one per line */
  function wrap(open, items, close, indent, width) {
    if (items.length === 0) return open.trimEnd() + close.trimStart();
    const one = open + ' ' + items.join(', ') + ' ' + close;
    if (one.length + indent <= width && !items.some((s) => s.includes('\n'))) return one;
    const pad = ' '.repeat(indent + 2);
    // Nested entries are already indented for their own depth
    return open + '\n' + items.map((s) => pad + s).join(',\n') + '\n' + ' '.repeat(indent) + close;
  }

  function inspect(value, options) {
    const opts = Object.assign({ depth: 2, width: 80, showHidden: false }, typeof options === 'object' && options ? options : {});
    const seen = [];

    function props(o, skipIndices) {
      const out = [];
      for (const k of Reflect.ownKeys(o)) {
        if (skipIndices && typeof k === 'string' && /^\d+$/.test(k)) continue;
        const d = Object.getOwnPropertyDescriptor(o, k);
        if (!d || (!d.enumerable && !opts.showHidden)) continue;
        let v;
        if ('value' in d) v = fmt(d.value, seen.length);
        else v = d.get && d.set ? '[Getter/Setter]' : d.get ? '[Getter]' : '[Setter]';
        out.push(key(k) + ': ' + v);
      }
      return out;
    }

    function fmt(v, level) {
      switch (typeof v) {
        case 'string': return level === 0 ? v : quote(v);
        case 'number': return Object.is(v, -0) ? '-0' : String(v);
        case 'bigint': return v + 'n';
        case 'symbol': return v.toString();
        case 'undefined': return 'undefined';
        case 'boolean': return String(v);
      }
      if (v === null) return 'null';
      if (seen.includes(v)) return '[Circular *1]';
      if (typeof v[custom] === 'function' && v[custom] !== inspect) {
        const r = v[custom](opts.depth - level, opts, inspect);
        if (typeof r === 'string') return r;
        return fmt(r, level);
      }
      const indent = level * 2;
      const deep = level > opts.depth;
      seen.push(v);
      try {
        if (typeof v === 'function') {
          const extra = props(v, false).filter((p) => !/^(prototype|length|name): /.test(p));
          return extra.length && !deep ? wrap(functionText(v) + ' {', extra, '}', indent, opts.width) : functionText(v);
        }
        if (Array.isArray(v)) {
          if (deep) return '[Array]';
          const items = [];
          let holes = 0;
          for (let i = 0; i < v.length; i++) {
            if (!(i in v)) { holes++; continue; }
            if (holes) { items.push('<' + holes + ' empty item' + (holes > 1 ? 's' : '') + '>'); holes = 0; }
            if (items.length >= 100) { items.push('... ' + (v.length - i) + ' more items'); break; }
            items.push(fmt(v[i], level + 1));
          }
          if (holes) items.push('<' + holes + ' empty item' + (holes > 1 ? 's' : '') + '>');
          return wrap('[', items.concat(props(v, true)), ']', indent, opts.width);
        }
        if (v instanceof Error) {
          const extra = props(v, false).filter((p) => !/^(stack|message): /.test(p));
          return extra.length && !deep ? errorText(v) + ' ' + wrap('{', extra, '}', indent, opts.width) : errorText(v);
        }
        if (v instanceof Date) return isNaN(v) ? 'Invalid Date' : v.toISOString();
        if (v instanceof RegExp) return v.toString();
        if (v instanceof Promise) return 'Promise { <unknown> }';
        if (v instanceof WeakMap || v instanceof WeakSet) return (v instanceof WeakMap ? 'WeakMap' : 'WeakSet') + ' { <items unknown> }';
        if (v instanceof Map) {
          if (deep) return '[Map]';
          const items = [...v].map(([k, x]) => fmt(k, level + 1) + ' => ' + fmt(x, level + 1));
          return wrap((constructorName(v) || 'Map') + '(' + v.size + ') {', items.concat(props(v, false)), '}', indent, opts.width);
        }
        if (v instanceof Set) {
          if (deep) return '[Set]';
          const items = [...v].map((x) => fmt(x, level + 1));
          return wrap((constructorName(v) || 'Set') + '(' + v.size + ') {', items.concat(props(v, false)), '}', indent, opts.width);
        }
        if (ArrayBuffer.isView(v) && !(v instanceof DataView)) {
          const name = constructorName(v) || typedArrayTag.call(v);
          const shown = Array.from(v.subarray(0, 100), (x) => fmt(x, level + 1));
          if (v.length > 100) shown.push('... ' + (v.length - 100) + ' more items');
          return wrap(name + '(' + v.length + ') [', shown, ']', indent, opts.width);
        }
        if (v instanceof ArrayBuffer) {
          const bytes = Array.from(new Uint8Array(v, 0, Math.min(v.byteLength, 50)), (b) => b.toString(16).padStart(2, '0'));
          return 'ArrayBuffer { [Uint8Contents]: <' + bytes.join(' ') + (v.byteLength > 50 ? ' ...' : '') + '>, byteLength: ' + v.byteLength + ' }';
        }
        for (const [Box, name] of [[Number, 'Number'], [String, 'String'], [Boolean, 'Boolean']]) {
          if (v instanceof Box) return '[' + name + ': ' + fmt(v.valueOf(), 1) + ']';
        }
        const proto = Object.getPrototypeOf(v);
        const tag = v[Symbol.toStringTag];
        let prefix = '';
        if (proto === null) prefix = '[Object: null prototype] ';
        else {
          const name = constructorName(v);
          if (name && name !== 'Object') prefix = name + ' ';
          if (typeof tag === 'string' && tag && tag !== name) prefix += '[' + tag + '] ';
        }
        if (deep) return '[' + (prefix.trim() || 'Object') + ']';
        return wrap(prefix + '{', props(v, false), '}', indent, opts.width);
      } finally {
        seen.pop();
      }
    }
    return fmt(value, 0);
  }
  inspect.custom = custom;

  /** printf-style: %s %d %i %f %j %o %O %c %% */
  function format(...args) {
    let out = '';
    let rest = args;
    if (typeof args[0] === 'string') {
      const f = args[0];
      let i = 1;
      out = f.replace(/%([sdifjoOc%])/g, (m, c) => {
        if (c === '%') return '%';
        if (i >= args.length) return m;
        const a = args[i++];
        switch (c) {
          case 's': return typeof a === 'string' ? a : typeof a === 'bigint' ? a + 'n' : typeof a === 'object' && a !== null ? inspect(a, { depth: 1 }) : String(a);
          case 'd': return typeof a === 'bigint' ? a + 'n' : typeof a === 'object' ? 'NaN' : String(Number(a));
          case 'i': return typeof a === 'bigint' ? a + 'n' : String(parseInt(a));
          case 'f': return String(parseFloat(a));
          case 'j': try { return JSON.stringify(a); } catch { return '[Circular]'; }
          case 'o': return inspect(a, { showHidden: true, depth: 4 });
          case 'O': return inspect(a);
          case 'c': return '';
        }
        return m;
      });
      rest = args.slice(i);
    } else if (args.length) {
      out = inspect(args[0]);
      rest = args.slice(1);
    }
    for (const a of rest) out += ' ' + (typeof a === 'string' ? a : inspect(a));
    return out;
  }

  let groupIndent = '';
  const counts = new Map();
  const timers = new Map();
  const write = (to, args) => {
    const text = format(...args);
    to((groupIndent ? text.replace(/^/gm, groupIndent) : text) + '\n');
  };
  const out = (...args) => write(stdio.out, args);
  const err = (...args) => write(stdio.err, args);

  function table(data) {
    if (data === null || typeof data !== 'object') return out(data);
    const rows = [...(data instanceof Map ? data : Object.entries(data))];
    const cols = [];
    for (const [, row] of rows) {
      if (row !== null && typeof row === 'object') for (const k of Object.keys(row)) if (!cols.includes(k)) cols.push(k);
    }
    const hasValues = rows.some(([, r]) => r === null || typeof r !== 'object');
    const head = ['(index)', ...cols, ...(hasValues ? ['Values'] : [])];
    const body = rows.map(([i, r]) => [String(i), ...cols.map((c) => (r !== null && typeof r === 'object' && c in r ? inspect(r[c], { depth: 0 }) : '')), ...(hasValues ? [r !== null && typeof r === 'object' ? '' : inspect(r, { depth: 0 })] : [])]);
    const widths = head.map((h, c) => Math.max(h.length, ...body.map((r) => r[c].length)) + 2);
    const line = (l, m, r) => l + widths.map((w) => '─'.repeat(w)).join(m) + r;
    const cells = (r) => '│' + r.map((s, c) => { const w = widths[c] - s.length; return ' '.repeat(w >> 1) + s + ' '.repeat(w - (w >> 1)); }).join('│') + '│';
    stdio.out([line('┌', '┬', '┐'), cells(head), line('├', '┼', '┤'), ...body.map(cells), line('└', '┴', '┘')].join('\n') + '\n');
  }

  const console = {
    log: out, info: out, debug: out,
    warn: err, error: err,
    trace: (...args) => err('Trace' + (args.length ? ': ' + format(...args) : '')),
    dir: (v, o) => out(inspect(v, o)),
    dirxml: out,
    table,
    assert: (ok, ...args) => { if (!ok) err('Assertion failed' + (args.length ? ': ' + format(...args) : '')); },
    count: (label = 'default') => { const n = (counts.get(label) || 0) + 1; counts.set(label, n); out(label + ': ' + n); },
    countReset: (label = 'default') => { counts.delete(label); },
    time: (label = 'default') => { timers.set(label, clock.now()); },
    timeLog: (label = 'default', ...args) => { if (timers.has(label)) out(label + ': ' + (clock.now() - timers.get(label)).toFixed(3) + 'ms', ...args); },
    timeEnd: (label = 'default') => { if (timers.has(label)) { out(label + ': ' + (clock.now() - timers.get(label)).toFixed(3) + 'ms'); timers.delete(label); } },
    group: (...args) => { if (args.length) out(...args); groupIndent += '  '; },
    groupCollapsed: (...args) => { if (args.length) out(...args); groupIndent += '  '; },
    groupEnd: () => { groupIndent = groupIndent.slice(2); },
  };
  Object.defineProperty(globalThis, 'console', { value: console, writable: true, configurable: true, enumerable: false });

  /** An error nobody caught (a timer callback, a microtask): print it and end the process, as Node and Deno do */
  function reportError(e) {
    if (typeof native.onUncaught === 'function' && native.onUncaught(e)) return;
    stdio.err('Uncaught ' + inspect(e) + '\n');
    stdio.exit(1);
  }
  Object.defineProperty(globalThis, 'reportError', { value: reportError, writable: true, configurable: true });
  Object.assign(native, { inspect, format, reportError });
})(globalThis.__j8_native);
