// node:events — EventEmitter.
(function (native) {
  'use strict';
  const kCapture = Symbol.for('nodejs.rejection');
  const errorMonitor = Symbol('events.errorMonitor');

  function EventEmitter(opts) { EventEmitter.init.call(this, opts); }
  EventEmitter.init = function () {
    if (this._events === undefined || this._events === Object.getPrototypeOf(this)._events) {
      this._events = Object.create(null);
      this._eventsCount = 0;
    }
    this._maxListeners = this._maxListeners || undefined;
  };
  EventEmitter.defaultMaxListeners = 10;
  EventEmitter.errorMonitor = errorMonitor;
  EventEmitter.captureRejectionSymbol = kCapture;

  const events = (self) => {
    if (!self._events) { self._events = Object.create(null); self._eventsCount = 0; }
    return self._events;
  };
  const check = (f) => { if (typeof f !== 'function') throw new TypeError('The "listener" argument must be of type function. Received ' + typeof f); };

  function add(self, name, listener, prepend) {
    check(listener);
    const ev = events(self);
    if (ev.newListener !== undefined) self.emit('newListener', name, listener.listener ? listener.listener : listener);
    const list = ev[name];
    if (list === undefined) { ev[name] = [listener]; self._eventsCount++; } else if (prepend) list.unshift(listener); else list.push(listener);
    const max = self.getMaxListeners();
    if (max > 0 && ev[name].length > max && !ev[name].warned) {
      ev[name].warned = true;
      if (typeof process === 'object' && process.emitWarning) process.emitWarning('Possible EventEmitter memory leak detected. ' + ev[name].length + ' ' + String(name) + ' listeners added. Use emitter.setMaxListeners() to increase limit', 'MaxListenersExceededWarning');
    }
    return self;
  }

  function onceWrapper(self, name, listener) {
    const w = function (...args) {
      self.removeListener(name, w);
      return listener.apply(self, args);
    };
    w.listener = listener;
    return w;
  }

  Object.assign(EventEmitter.prototype, {
    _events: undefined,
    _eventsCount: 0,
    _maxListeners: undefined,
    setMaxListeners(n) { this._maxListeners = n; return this; },
    getMaxListeners() { return this._maxListeners === undefined ? EventEmitter.defaultMaxListeners : this._maxListeners; },
    emit(name, ...args) {
      const ev = events(this);
      if (name === 'error' && ev[errorMonitor] !== undefined) this.emit(errorMonitor, ...args);
      const list = ev[name];
      if (list === undefined) {
        if (name === 'error') {
          const er = args[0];
          if (er instanceof Error) throw er;
          const e = new Error('Unhandled error.' + (er === undefined ? '' : ' (' + native.inspect(er) + ')'));
          e.context = er;
          throw e;
        }
        return false;
      }
      for (const f of list.slice()) f.apply(this, args);
      return true;
    },
    on(name, f) { return add(this, name, f, false); },
    addListener(name, f) { return add(this, name, f, false); },
    prependListener(name, f) { return add(this, name, f, true); },
    once(name, f) { check(f); return add(this, name, onceWrapper(this, name, f), false); },
    prependOnceListener(name, f) { check(f); return add(this, name, onceWrapper(this, name, f), true); },
    removeListener(name, f) {
      check(f);
      const ev = events(this);
      const list = ev[name];
      if (list === undefined) return this;
      for (let i = list.length - 1; i >= 0; i--) {
        if (list[i] === f || list[i].listener === f) {
          list.splice(i, 1);
          if (list.length === 0) { delete ev[name]; this._eventsCount--; }
          if (ev.removeListener !== undefined) this.emit('removeListener', name, f);
          break;
        }
      }
      return this;
    },
    removeAllListeners(name) {
      const ev = events(this);
      if (name === undefined) { this._events = Object.create(null); this._eventsCount = 0; } else if (ev[name] !== undefined) { delete ev[name]; this._eventsCount--; }
      return this;
    },
    listeners(name) { return (events(this)[name] || []).map((f) => f.listener || f); },
    rawListeners(name) { return (events(this)[name] || []).slice(); },
    listenerCount(name, f) { const l = events(this)[name] || []; return f === undefined ? l.length : l.filter((x) => x === f || x.listener === f).length; },
    eventNames() { return Reflect.ownKeys(events(this)); },
  });
  EventEmitter.prototype.off = EventEmitter.prototype.removeListener;
  EventEmitter.EventEmitter = EventEmitter;
  EventEmitter.listenerCount = (emitter, name) => emitter.listenerCount(name);
  EventEmitter.getEventListeners = (target, name) => (typeof target.listeners === 'function' ? target.listeners(name) : []);

  /** Resolves with the arguments of the next `name` event (rejects on 'error') */
  EventEmitter.once = (emitter, name, options = {}) => new Promise((resolve, reject) => {
    if (typeof emitter.addEventListener === 'function' && !(emitter instanceof EventEmitter)) {
      emitter.addEventListener(name, (...args) => resolve(args), { once: true, signal: options.signal });
      return;
    }
    const onError = (e) => { emitter.removeListener(name, onEvent); reject(e); };
    const onEvent = (...args) => { if (name !== 'error') emitter.removeListener('error', onError); resolve(args); };
    emitter.once(name, onEvent);
    if (name !== 'error') emitter.once('error', onError);
    if (options.signal) options.signal.addEventListener('abort', () => { emitter.removeListener(name, onEvent); emitter.removeListener('error', onError); reject(options.signal.reason); }, { once: true });
  });

  /** An async iterator over `name` events */
  EventEmitter.on = (emitter, name) => {
    const queue = [];
    const waiting = [];
    let failed = null;
    const push = (...args) => { if (waiting.length) waiting.shift().resolve({ value: args, done: false }); else queue.push(args); };
    const fail = (e) => { failed = e; while (waiting.length) waiting.shift().reject(e); };
    emitter.on(name, push);
    if (name !== 'error') emitter.on('error', fail);
    return {
      next() {
        if (queue.length) return Promise.resolve({ value: queue.shift(), done: false });
        if (failed) return Promise.reject(failed);
        return new Promise((resolve, reject) => waiting.push({ resolve, reject }));
      },
      return() { emitter.removeListener(name, push); emitter.removeListener('error', fail); return Promise.resolve({ value: undefined, done: true }); },
      [Symbol.asyncIterator]() { return this; },
    };
  };

  native.define('events', () => EventEmitter);
  native.EventEmitter = EventEmitter;
})(globalThis.__j8_native);
