// DOM events: Event / CustomEvent / EventTarget, AbortController / AbortSignal, DOMException.
(function (native) {
  'use strict';

  const DOM_CODES = { IndexSizeError: 1, NotFoundError: 8, NotSupportedError: 9, InvalidStateError: 11, SyntaxError: 12, InvalidAccessError: 15, TimeoutError: 23, DataCloneError: 25, AbortError: 20 };

  class DOMException extends Error {
    constructor(message = '', name = 'Error') {
      super(message);
      Object.defineProperty(this, 'name', { value: String(name), configurable: true, writable: true });
    }
    get code() { return DOM_CODES[this.name] || 0; }
  }

  class Event {
    #stop = false;
    #immediate = false;
    #canceled = false;
    constructor(type, init = {}) {
      if (arguments.length === 0) throw new TypeError("Failed to construct 'Event': 1 argument required");
      this.type = String(type);
      this.bubbles = !!init.bubbles;
      this.cancelable = !!init.cancelable;
      this.composed = !!init.composed;
      this.timeStamp = native.clock.now();
      this.target = null;
      this.currentTarget = null;
      this.isTrusted = false;
    }
    get defaultPrevented() { return this.#canceled; }
    preventDefault() { if (this.cancelable) this.#canceled = true; }
    stopPropagation() { this.#stop = true; }
    stopImmediatePropagation() { this.#stop = true; this.#immediate = true; }
    get cancelBubble() { return this.#stop; }
    static stopped(e) { return e.#immediate; }
  }

  class CustomEvent extends Event {
    constructor(type, init = {}) {
      super(type, init);
      this.detail = init.detail === undefined ? null : init.detail;
    }
  }

  const listeners = new WeakMap();

  class EventTarget {
    constructor() { listeners.set(this, new Map()); }
    addEventListener(type, callback, options) {
      if (callback == null) return;
      const opts = typeof options === 'boolean' ? { capture: options } : options || {};
      const list = this.#list(type, true);
      if (list.some((l) => l.callback === callback && l.capture === !!opts.capture)) return;
      const entry = { callback, capture: !!opts.capture, once: !!opts.once };
      if (opts.signal) {
        if (opts.signal.aborted) return;
        opts.signal.addEventListener('abort', () => this.removeEventListener(type, callback, opts));
      }
      list.push(entry);
    }
    removeEventListener(type, callback, options) {
      const capture = typeof options === 'boolean' ? options : !!(options && options.capture);
      const list = this.#list(type, false);
      if (!list) return;
      const i = list.findIndex((l) => l.callback === callback && l.capture === capture);
      if (i >= 0) list.splice(i, 1);
    }
    dispatchEvent(event) {
      if (!(event instanceof Event)) throw new TypeError("parameter 1 is not of type 'Event'");
      event.target = this;
      event.currentTarget = this;
      for (const l of [...(this.#list(event.type, false) || [])]) {
        if (l.once) this.removeEventListener(event.type, l.callback, l);
        try {
          if (typeof l.callback === 'function') l.callback.call(this, event);
          else l.callback.handleEvent(event);
        } catch (e) {
          native.reportError(e);
        }
        if (Event.stopped(event)) break;
      }
      event.currentTarget = null;
      return !event.defaultPrevented;
    }
    #list(type, create) {
      let map = listeners.get(this);
      if (!map) { map = new Map(); listeners.set(this, map); }
      let list = map.get(String(type));
      if (!list && create) { list = []; map.set(String(type), list); }
      return list;
    }
  }

  class AbortSignal extends EventTarget {
    #aborted = false;
    #reason = undefined;
    constructor(key) {
      if (key !== native) throw new TypeError('Illegal constructor');
      super();
      this.onabort = null;
    }
    get aborted() { return this.#aborted; }
    get reason() { return this.#reason; }
    throwIfAborted() { if (this.#aborted) throw this.#reason; }
    static abort(reason) {
      const s = new AbortSignal(native);
      AbortSignal.fire(s, reason);
      return s;
    }
    static timeout(ms) {
      const s = new AbortSignal(native);
      setTimeout(() => AbortSignal.fire(s, new DOMException('The operation timed out.', 'TimeoutError')), ms);
      return s;
    }
    static any(signals) {
      const s = new AbortSignal(native);
      for (const t of signals) {
        if (t.aborted) { AbortSignal.fire(s, t.reason); break; }
        t.addEventListener('abort', () => AbortSignal.fire(s, t.reason), { once: true });
      }
      return s;
    }
    static fire(s, reason) {
      if (s.#aborted) return;
      s.#aborted = true;
      s.#reason = reason === undefined ? new DOMException('This operation was aborted', 'AbortError') : reason;
      const e = new Event('abort');
      if (typeof s.onabort === 'function') s.onabort(e);
      s.dispatchEvent(e);
    }
  }

  class AbortController {
    #signal = new AbortSignal(native);
    get signal() { return this.#signal; }
    abort(reason) { AbortSignal.fire(this.#signal, reason); }
  }

  for (const c of [DOMException, Event, CustomEvent, EventTarget, AbortSignal, AbortController]) {
    Object.defineProperty(globalThis, c.name, { value: c, writable: true, configurable: true });
  }
})(globalThis.__j8_native);
