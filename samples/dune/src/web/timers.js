// Timers: setTimeout / setInterval / clearTimeout / clearInterval, queueMicrotask.
// Each armed timer waits on a native Timer (a promise the event loop settles); whichever wait fires runs every timer
// that is due, earliest first, so timers fire in order even when their waits wake out of order. The Node layer builds
// its Timeout objects on `native.timers`.
(function (native) {
  'use strict';
  const { clock, Timer } = native;
  const active = new Map();
  let lastId = 0;
  let order = 0;
  let draining = false;

  function wait(t, ms) {
    if (t.wait) t.wait.cancel();
    t.wait = new Timer();
    t.wait.wait(ms).then((fired) => { if (fired) drain(); });
  }

  function arm(t) {
    t.due = clock.now() + t.delay;
    t.order = ++order;
    if (t.ref) wait(t, t.delay);
  }

  function next(now) {
    let best = null;
    for (const t of active.values()) {
      if (t.due <= now && (!best || t.due < best.due || (t.due === best.due && t.order < best.order))) best = t;
    }
    return best;
  }

  /** Runs one due timer, then (after the microtasks it queued) the next */
  function drain() {
    if (draining) return;
    const t = next(clock.now());
    if (!t) return;
    draining = true;
    if (t.repeat) arm(t);
    else timers.clear(t.id);
    try {
      t.callback.apply(globalThis, t.args);
    } catch (e) {
      native.reportError(e);
    } finally {
      draining = false;
    }
    Promise.resolve().then(drain);
  }

  const timers = {
    add(callback, delay, args, repeat) {
      if (typeof callback !== 'function') throw new TypeError('The "callback" argument must be of type function');
      delay = Number(delay);
      if (!(delay >= 1 && delay <= 2147483647)) delay = repeat ? 1 : 0;
      const t = { id: ++lastId, callback, args, delay, repeat, ref: true, wait: null, due: 0, order: 0 };
      active.set(t.id, t);
      arm(t);
      return t.id;
    },
    clear(id) {
      const t = active.get(id);
      if (!t) return;
      if (t.wait) t.wait.cancel();
      active.delete(id);
    },
    has(id) { return active.has(id); },
    /** An unreferenced timer does not keep the process alive; it still fires if something else does */
    ref(id, on) {
      const t = active.get(id);
      if (!t || t.ref === on) return;
      t.ref = on;
      if (on) wait(t, Math.max(0, t.due - clock.now()));
      else if (t.wait) { t.wait.cancel(); t.wait = null; }
    },
    refresh(id) { const t = active.get(id); if (t) arm(t); },
  };

  const define = (name, value) => Object.defineProperty(globalThis, name, { value, writable: true, configurable: true });
  define('setTimeout', (callback, delay, ...args) => timers.add(callback, delay, args, false));
  define('setInterval', (callback, delay, ...args) => timers.add(callback, delay, args, true));
  define('clearTimeout', (id) => timers.clear(+id));
  define('clearInterval', (id) => timers.clear(+id));
  define('queueMicrotask', (callback) => {
    if (typeof callback !== 'function') throw new TypeError('The "callback" argument must be of type function');
    Promise.resolve().then(() => { try { callback(); } catch (e) { native.reportError(e); } });
  });
  native.timers = timers;
})(globalThis.__j8_native);
