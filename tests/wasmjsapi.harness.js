var self = globalThis;
var __tests = [];
var __setupError = null;
var __cleanups = [];

function AssertionError(message) {
  this.message = message;
}
AssertionError.prototype.toString = function () { return "AssertionError: " + this.message; };

function format_value(v) {
  if (typeof v === "string") return JSON.stringify(v);
  if (typeof v === "bigint") return String(v) + "n";
  if (typeof v === "symbol") return String(v);
  if (Object.is(v, -0)) return "-0";
  if (typeof v === "function") return "function \"" + v.name + "\"";
  if (v === null || typeof v !== "object") return String(v);
  try { return Object.prototype.toString.call(v); } catch (e) { return "[object]"; }
}

function __fail(description, message) {
  throw new AssertionError((description ? description + ": " : "") + message);
}

function __same(a, b) {
  return Object.is(a, b);
}

function assert_true(v, description) { if (v !== true) __fail(description, "expected true got " + format_value(v)); }
function assert_false(v, description) { if (v !== false) __fail(description, "expected false got " + format_value(v)); }
function assert_equals(a, b, description) { if (!__same(a, b)) __fail(description, "expected " + format_value(b) + " but got " + format_value(a)); }
function assert_not_equals(a, b, description) { if (__same(a, b)) __fail(description, "got disallowed value " + format_value(b)); }
function assert_unreached(description) { __fail(description, "reached unreachable code"); }
var assert_not_reached = assert_unreached;
function assert_own_property(o, p, description) { if (!Object.prototype.hasOwnProperty.call(o, p)) __fail(description, "expected property " + format_value(p) + " missing"); }
function assert_not_own_property(o, p, description) { if (Object.prototype.hasOwnProperty.call(o, p)) __fail(description, "unexpected property " + format_value(p) + " is found"); }
function assert_class_string(o, cls, description) {
  var s = Object.prototype.toString.call(o);
  assert_equals(s, "[object " + cls + "]", description);
}
function assert_array_equals(a, b, description) {
  if (typeof a !== "object" || a === null || !("length" in a)) __fail(description, "value is " + format_value(a) + ", expected array");
  if (a.length !== b.length) __fail(description, "lengths differ, expected " + b.length + " got " + a.length);
  for (var i = 0; i < a.length; i++) {
    if (!__same(a[i], b[i])) __fail(description, "expected " + format_value(b[i]) + " but got " + format_value(a[i]) + " at index " + i);
  }
}
function assert_throws_js(ctor, fn, description) {
  try {
    fn();
  } catch (e) {
    if (typeof e !== "object" || e === null) __fail(description, "threw " + format_value(e) + ", expected an object");
    if (e.constructor !== ctor) __fail(description, "threw " + format_value(e) + " (" + (e.constructor && e.constructor.name) + ": " + e.message + "), expected " + ctor.name);
    return;
  }
  __fail(description, "did not throw, expected " + ctor.name);
}
function assert_throws(code, fn, description) {
  try {
    fn();
  } catch (e) {
    if (code !== null && typeof code === "object" && e.constructor !== code.constructor) __fail(description, "threw " + format_value(e) + ", expected " + code.constructor.name);
    return;
  }
  __fail(description, "did not throw");
}
function assert_throws_exactly(value, fn, description) {
  try {
    fn();
  } catch (e) {
    if (!__same(e, value)) __fail(description, "threw " + format_value(e) + ", expected " + format_value(value));
    return;
  }
  __fail(description, "did not throw");
}

function __Test(name) {
  this.name = name;
  this.cleanups = [];
}
__Test.prototype.step_func = function (f, thisObj) {
  return function () { return f.apply(thisObj === undefined ? this : thisObj, arguments); };
};
__Test.prototype.step_func_done = __Test.prototype.step_func;
__Test.prototype.step = function (f, thisObj) {
  var args = Array.prototype.slice.call(arguments, 2);
  return f.apply(thisObj === undefined ? this : thisObj, args);
};
__Test.prototype.unreached_func = function (description) {
  return function () { assert_unreached(description); };
};
__Test.prototype.add_cleanup = function (f) { this.cleanups.push(f); };
__Test.prototype.done = function () {};

function __report(name, error) {
  if (error === undefined) {
    console.log("PASS\t" + name);
  } else {
    var m;
    try { m = error instanceof AssertionError ? error.message : String(error); } catch (e) { m = "?"; }
    console.log("FAIL\t" + name + "\t" + m.replace(/[\t\n]/g, " "));
  }
}

function __cleanup(t) {
  for (var i = 0; i < t.cleanups.length; i++) {
    try { t.cleanups[i](); } catch (e) {}
  }
}

function test(fn, name) {
  var t = new __Test(name);
  if (__setupError !== null) { __report(name, __setupError); return; }
  try {
    fn.call(t, t);
    __report(name);
  } catch (e) {
    __report(name, e);
  }
  __cleanup(t);
}

function promise_test(fn, name) {
  __tests.push({ fn: fn, name: name });
}

function promise_rejects_js(t, ctor, promise, description) {
  return promise.then(function () {
    __fail(description, "should have rejected");
  }, function (e) {
    if (typeof e !== "object" || e === null || e.constructor !== ctor) __fail(description, "rejected with " + format_value(e) + ", expected " + ctor.name);
  });
}

function promise_rejects_exactly(t, value, promise, description) {
  return promise.then(function () {
    __fail(description, "should have rejected");
  }, function (e) {
    if (!__same(e, value)) __fail(description, "rejected with " + format_value(e));
  });
}

function setup(f) {
  if (typeof f !== "function") return;
  try { f(); } catch (e) { __setupError = e; }
}

function done() {}

function async_test(fn, name) {
  __report(typeof fn === "string" ? fn : name, "async_test is not supported");
}

function __run() {
  var p = Promise.resolve();
  __tests.forEach(function (entry) {
    p = p.then(function () {
      var t = new __Test(entry.name);
      if (__setupError !== null) { __report(entry.name, __setupError); return; }
      var r;
      try { r = Promise.resolve(entry.fn.call(t, t)); } catch (e) { r = Promise.reject(e); }
      return r.then(function () { __report(entry.name); }, function (e) { __report(entry.name, e); }).then(function () { __cleanup(t); });
    });
  });
  p.then(function () { console.log("DONE"); });
}
