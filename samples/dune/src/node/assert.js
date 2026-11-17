// node:assert (and assert/strict).
(function (native) {
  'use strict';
  const { inspect, deepEqual } = native;

  class AssertionError extends Error {
    constructor(options = {}) {
      const { message, actual, expected, operator } = options;
      super(message !== undefined ? String(message) : inspect(actual) + ' ' + operator + ' ' + inspect(expected));
      this.name = 'AssertionError';
      this.code = 'ERR_ASSERTION';
      this.actual = actual;
      this.expected = expected;
      this.operator = operator;
      this.generatedMessage = message === undefined;
    }
  }

  function fail(message, actual, expected, operator, defaultText) {
    if (message instanceof Error) throw message;
    throw new AssertionError({ message: message === undefined ? defaultText : message, actual, expected, operator });
  }

  /** Whether `e` matches `expected`: a class, a RegExp on the message, a predicate, or an object of fields */
  function matches(e, expected) {
    if (expected === undefined) return true;
    if (expected instanceof RegExp) return expected.test(String(e && e.message !== undefined ? e : e));
    if (typeof expected === 'function') {
      if (expected.prototype !== undefined && e instanceof expected) return true;
      if (Error.isPrototypeOf(expected) || expected === Error) return false;
      return expected.call({}, e) === true;
    }
    if (typeof expected === 'object' && expected !== null) {
      return Object.keys(expected).every((k) => (expected[k] instanceof RegExp && typeof e[k] === 'string' ? expected[k].test(e[k]) : deepEqual(e[k], expected[k], true)));
    }
    return false;
  }

  function make(strict) {
    const assert = (value, message) => assert.ok(value, message);
    Object.assign(assert, {
      AssertionError,
      ok(value, message) { if (!value) fail(message, value, true, '==', 'The expression evaluated to a falsy value:\n\n  assert.ok(' + inspect(value) + ')\n'); },
      equal(a, b, m) { if (strict ? !Object.is(a, b) : !(a == b || (a !== a && b !== b))) fail(m, a, b, strict ? 'strictEqual' : '==', strict ? 'Expected values to be strictly equal:\n\n' + inspect(a) + ' !== ' + inspect(b) + '\n' : undefined); },
      notEqual(a, b, m) { if (strict ? Object.is(a, b) : a == b) fail(m, a, b, strict ? 'notStrictEqual' : '!='); },
      strictEqual(a, b, m) { if (!Object.is(a, b)) fail(m, a, b, 'strictEqual', 'Expected values to be strictly equal:\n\n' + inspect(a) + ' !== ' + inspect(b) + '\n'); },
      notStrictEqual(a, b, m) { if (Object.is(a, b)) fail(m, a, b, 'notStrictEqual', 'Expected "actual" to be strictly unequal to: ' + inspect(b)); },
      deepEqual(a, b, m) { if (!deepEqual(a, b, strict)) fail(m, a, b, strict ? 'deepStrictEqual' : 'deepEqual', 'Expected values to be ' + (strict ? 'strictly ' : 'loosely ') + 'deep-equal:\n\n' + inspect(a) + '\n\nshould equal\n\n' + inspect(b)); },
      notDeepEqual(a, b, m) { if (deepEqual(a, b, strict)) fail(m, a, b, strict ? 'notDeepStrictEqual' : 'notDeepEqual', 'Expected "actual" not to be deep-equal to: ' + inspect(b)); },
      deepStrictEqual(a, b, m) { if (!deepEqual(a, b, true)) fail(m, a, b, 'deepStrictEqual', 'Expected values to be strictly deep-equal:\n\n' + inspect(a) + '\n\nshould equal\n\n' + inspect(b)); },
      notDeepStrictEqual(a, b, m) { if (deepEqual(a, b, true)) fail(m, a, b, 'notDeepStrictEqual', 'Expected "actual" not to be strictly deep-equal to: ' + inspect(b)); },
      throws(f, expected, m) {
        if (typeof expected === 'string') { m = expected; expected = undefined; }
        try { f(); } catch (e) {
          if (!matches(e, expected)) throw e;
          return;
        }
        fail(m, undefined, expected, 'throws', 'Missing expected exception' + (m ? ': ' + m : '.'));
      },
      doesNotThrow(f, expected, m) {
        try { f(); } catch (e) { fail(typeof expected === 'string' ? expected : m, e, expected, 'doesNotThrow', 'Got unwanted exception.\nActual message: "' + (e && e.message) + '"'); }
      },
      async rejects(p, expected, m) {
        if (typeof expected === 'string') { m = expected; expected = undefined; }
        try { await (typeof p === 'function' ? p() : p); } catch (e) {
          if (!matches(e, expected)) throw e;
          return;
        }
        fail(m, undefined, expected, 'rejects', 'Missing expected rejection' + (m ? ': ' + m : '.'));
      },
      async doesNotReject(p, expected, m) {
        try { await (typeof p === 'function' ? p() : p); } catch (e) { fail(typeof expected === 'string' ? expected : m, e, expected, 'doesNotReject', 'Got unwanted rejection.\nActual message: "' + (e && e.message) + '"'); }
      },
      match(s, re, m) { if (!re.test(s)) fail(m, s, re, 'match', 'The input did not match the regular expression ' + re + '. Input:\n\n' + inspect(s) + '\n'); },
      doesNotMatch(s, re, m) { if (re.test(s)) fail(m, s, re, 'doesNotMatch', 'The input was expected to not match the regular expression ' + re + '. Input:\n\n' + inspect(s) + '\n'); },
      fail(m = 'Failed') { fail(m, undefined, undefined, 'fail'); },
      ifError(v) { if (v !== null && v !== undefined) { if (v instanceof Error) throw v; fail(undefined, v, null, 'ifError', 'ifError got unwanted exception: ' + inspect(v)); } },
    });
    if (strict) { assert.equal = assert.strictEqual; assert.notEqual = assert.notStrictEqual; assert.deepEqual = assert.deepStrictEqual; assert.notDeepEqual = assert.notDeepStrictEqual; }
    return assert;
  }

  native.define('assert', () => {
    const assert = make(false);
    assert.strict = make(true);
    assert.strict.strict = assert.strict;
    return assert;
  });
  native.define('assert/strict', () => native.builtin('assert').strict);
})(globalThis.__j8_native);
