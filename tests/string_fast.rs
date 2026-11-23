#![cfg(any(target_arch = "x86_64", target_arch = "aarch64"))]

use j8builtins::Builtins;
use j8executor::Executor;
use j8framework::{CreateParams, Isolate, Source};
use j8platform::DefaultPlatform;

#[derive(Clone, Copy)]
enum Tier {
    Interpreter,
    Immediate,
    Default,
}

impl Tier {
    fn label(self) -> &'static str {
        match self {
            Self::Interpreter => "interpreter",
            Self::Immediate => "threshold 0",
            Self::Default => "default threshold",
        }
    }
}

struct Case {
    what: &'static str,
    text: &'static str,
    want: &'static str,
}

const CASES: &[Case] = &[
    Case {
        what: "string and number in either order, including -0, fractions, non-finite and exponent forms",
        text: "function nums() { const xs = [0, -0, 1, -4, 12, 1.5, NaN, Infinity, -Infinity, 1e21, 1e-7, 9007199254740991, -9007199254740991, 2 ** 53]; const out = []; for (const x of xs) out.push('k' + x, x + 'k'); out.push('' + (-0), '' + '', 'a' + '', '' + 'b'); return out.join(','); } nums()",
        want: "k0,0k,k0,0k,k1,1k,k-4,-4k,k12,12k,k1.5,1.5k,kNaN,NaNk,kInfinity,Infinityk,k-Infinity,-Infinityk,k1e+21,1e+21k,k1e-7,1e-7k,k9007199254740991,9007199254740991k,k-9007199254740991,-9007199254740991k,k9007199254740992,9007199254740992k,0,,a,b",
    },
    Case {
        what: "boolean, null and undefined concatenate, and numbers still add",
        text: "function prim() { return ['a' + true, true + 'a', 'a' + false, false + 'a', 'a' + null, null + 'a', 'a' + undefined, undefined + 'a', 1 + 1, 1 + '1', '1' + 1, true + 1, 1 + true, null + 1, undefined + 1, '' + (-0)].join('|'); } prim()",
        want: "atrue|truea|afalse|falsea|anull|nulla|aundefined|undefineda|2|11|11|2|2|1|NaN|0",
    },
    Case {
        what: "symbol and bigint errors, bigint concatenated with a string, and nullish property errors",
        text: "function err(f) { try { return String(f()); } catch (e) { return e.name + ':' + e.message; } } function errors() { const sym = Symbol('s'); return [err(() => 'a' + sym), err(() => sym + 'k'), err(() => 1n + 'a'), err(() => 'a' + 1n), err(() => 1n + 1), err(() => 1 + 1n), String(1n + 2n), err(() => undefined.length), err(() => null.length), err(() => undefined.foo), err(() => null.foo)].join('|'); } errors()",
        want: "TypeError:Cannot convert a Symbol value to a string|TypeError:Cannot convert a Symbol value to a string|1a|a1|TypeError:Cannot mix BigInt and other types, use explicit conversions|TypeError:Cannot mix BigInt and other types, use explicit conversions|3|TypeError:Cannot read properties of undefined (reading 'length')|TypeError:Cannot read properties of null (reading 'length')|TypeError:Cannot read properties of undefined (reading 'foo')|TypeError:Cannot read properties of null (reading 'foo')",
    },
    Case {
        what: "lone surrogates, a pair split across two pieces, indexing a non-ASCII string, and fromCharCode",
        text: "function units(s) { let o = String(s.length); for (let i = 0; i < s.length; i++) o += ':' + s.charCodeAt(i); return o; } function sur() { const hi = String.fromCharCode(0xD800); const lo = String.fromCharCode(0xDE00); const pair = hi + lo; const text = 'a😀b'; let idx = ''; for (let i = 0; i < text.length; i++) idx += (i ? ',' : '') + text[i].charCodeAt(0); const codes = [0, 65, 127, 128, 255, 256, 0xD800, 0x10041, -1]; const shown = codes.map(c => { const s = String.fromCharCode(c); return s.length + ':' + s.charCodeAt(0); }).join('|'); const obj = String.fromCharCode({ valueOf() { return 70; } }); return units(hi) + '|' + units(pair) + '|' + [...pair].length + '|' + idx + '|' + shown + '|' + String.fromCharCode('66').charCodeAt(0) + '|' + obj.length + ':' + obj.charCodeAt(0) + '|' + String.fromCharCode().length + '|' + String.fromCharCode(65, 66, 67); } sur()",
        want: "1:55296|2:55296:56832|1|97,55357,56832,98|1:0|1:65|1:127|1:128|1:255|1:256|1:55296|1:65|1:65535|66|1:70|0|ABC",
    },
    Case {
        what: "length of empty, ASCII, non-ASCII, lone-surrogate, view and one-unit strings",
        text: "function lens() { const view = 'x'.repeat(80).slice(10, 50); const cut = 'y'.repeat(200).slice(20, 180); return ['', 'abc', 'é', String.fromCharCode(0xD800)].map(s => s.length).join(',') + ',' + view.length + ',' + cut.length + ',' + ('k' + 1).length + ',' + (1 + 'k').length; } lens()",
        want: "0,3,1,1,40,160,2,2",
    },
    Case {
        what: "growth past the buffer thresholds, two continuations, and appending a string to itself",
        text: "function grow() { let s = ''; for (let i = 0; i < 300; i++) s += 'a'; let t = s; for (let i = 0; i < 20; i++) t += i % 10; const prefix = 'm'.repeat(200); const one = prefix + 'a'; const other = prefix + 'b'; let self = 'q'.repeat(80); self += self; let fork = 'z'.repeat(150); const left = fork + 'a'; fork += 'b'; return [s.length, s[0], s[299], t.length, t.slice(-5), one.endsWith('a'), other.endsWith('b'), one.slice(0, 200) === other.slice(0, 200), self === 'q'.repeat(160), left.endsWith('a'), fork.endsWith('b'), left.slice(0, 150) === 'z'.repeat(150), fork.slice(0, 150) === 'z'.repeat(150)].join(','); } grow()",
        want: "300,a,a,320,56789,true,true,true,true,true,true,true,true",
    },
    Case {
        what: "a concatenation used as a Map key, compared, sliced, split and passed to JSON and a regex",
        text: "function use() { const s = 'k' + 7 + 'é'; const m = new Map(); m.set(s, 1); m.set('k7é', 2); const a = 'b' + 3; return [m.get('k7é'), m.size, a === 'b3', a < 'b4', a < 'b2', JSON.stringify(s), /é/.test(s), s.slice(1, 3), s.split('7').join('-'), s.indexOf('é')].join(','); } use()",
        want: "2,1,true,true,false,\"k7é\",true,7é,k-é,2",
    },
    Case {
        what: "an object operand that writes a global is visible to the next addition",
        text: "function epoch() { var g = 1; const o = { valueOf() { g = 5; return 'z'; } }; let s = 0; let acc = ''; for (let i = 0; i < 100; i++) { s += g; acc = (i < 50 ? 'a' : o) + 'b'; } return s + ':' + g + ':' + acc; } epoch()",
        want: "296:5:zb",
    },
    Case {
        what: "length and a method on a string, a replaced method, a strict getter and a property added later",
        text: "function methods() { function len(x) { return x.length; } function call(x) { return x.slice(0, 1); } let n = 0; let p = ''; for (let i = 0; i < 200; i++) { const s = i < 150 ? 'abcd' : 'WXYZ'; n += len(s); p = call(s); } const saved = String.prototype.slice; String.prototype.slice = function () { return 'Q' + this; }; const after = call('abcd'); String.prototype.slice = saved; function read(x) { 'use strict'; return x.extra; } let got = ''; for (let i = 0; i < 100; i++) { if (i === 40) Object.defineProperty(String.prototype, 'extra', { get() { 'use strict'; return 'G' + typeof this + this; }, configurable: true }); got = read('xy'); } delete String.prototype.extra; function miss(x) { return x.nope; } let added = 'start'; for (let i = 0; i < 80; i++) { if (i === 30) String.prototype.nope = 'Z'; added = String(miss('ab')); } delete String.prototype.nope; return n + ':' + p + ':' + after + ':' + got + ':' + added; } methods()",
        want: "800:W:Qabcd:Gstringxy:Z",
    },
    Case {
        what: "one length site alternates a string, an array, a plain object and a String object",
        text: "function alt() { function len(x) { return x.length; } const a = ['a', 'bb', 'ccc']; const o = { length: 9 }; const s = 'wxyz'; let out = 0; for (let i = 0; i < 300; i++) { const x = i % 3 === 0 ? s : i % 3 === 1 ? a : o; out = len(x); } return out + ':' + len(s) + ':' + len(a) + ':' + len(o) + ':' + len(new String('x')); } alt()",
        want: "9:4:3:9:1",
    },
    Case {
        what: "an index and a missing name on a string",
        text: "function idx() { function at(s) { return s[3] + ':' + s.nope + ':' + s.length; } let r = ''; for (let i = 0; i < 10; i++) r = at('abcd'); return r; } idx()",
        want: "d:undefined:4",
    },
    Case {
        what: "undefined and null receivers throw after a string length site has run",
        text: "function bad() { function len(x) { return x.length; } let r = ''; for (let i = 0; i < 50; i++) r = String(len('ab')); try { len(undefined); r += ':no'; } catch (e) { r += ':' + e.name; } try { len(null); r += ':no'; } catch (e) { r += ':' + e.name; } return r; } bad()",
        want: "2:TypeError:TypeError",
    },
    Case {
        what: "a repeated constant compares equal and is a Map key",
        text: "function cons() { function word() { return 'hello-world'; } const m = new Map(); m.set(word(), 1); const a = word(); const b = word(); return (a === b) + ':' + m.get('hello-world') + ':' + (a === 'hello-world') + ':' + ('k' === 'k'); } cons()",
        want: "true:1:true:true",
    },
    Case {
        what: "two continuations of one prefix stay right while the prefix grows",
        text: "function forks() { let ok = 0; let s = 'p'.repeat(40); for (let i = 0; i < 100; i++) { const t = s + 'a'; const u = s + 'b'; if (t.endsWith('a') && u.endsWith('b') && t.length === s.length + 1 && u.slice(0, -1) === s && t < u) ok++; s += 'c'; } return ok + ':' + s.length + ':' + s.endsWith('c'); } forks()",
        want: "100:140:true",
    },
    Case {
        what: "ToPrimitive order for Symbol.toPrimitive, valueOf and toString",
        text: "function order() { const log = []; const o = { [Symbol.toPrimitive](h) { log.push('p' + h); return 'P'; }, valueOf() { log.push('v'); return 'V'; }, toString() { log.push('s'); return 'S'; } }; const a = o + 'x'; const q = { valueOf() { log.push('v'); return 2; }, toString() { log.push('s'); return 'S'; } }; const b = 'n' + q; const r = { toString() { log.push('s2'); return 'T'; } }; const c = r + 'z'; return a + '|' + b + '|' + c + '|' + log.join(','); } order()",
        want: "Px|n2|Tz|pdefault,v,s2",
    },
    Case {
        what: "appending numbers onto a long string",
        text: "function appendNum() { let s = 'k'.repeat(120); for (let i = 0; i < 30; i++) s += i; return s.length + ':' + s.slice(120, 126) + ':' + s.slice(-4); } appendNum()",
        want: "170:012345:2829",
    },
    Case {
        what: "fromCharCode of a small integer in a loop, and values outside the uint16 range",
        text: "function chars() { let n = 0; let last = ''; for (let i = 0; i < 200; i++) { const s = String.fromCharCode(97 + (i % 26)); n += s.length; last = s; } return n + ':' + last + ':' + String.fromCharCode(0x10041) + ':' + String.fromCharCode(-1).charCodeAt(0); } chars()",
        want: "200:r:A:65535",
    },
    Case {
        what: "+= of a string and a number",
        text: "function plusEq() { let s = 'k'; s += 12; s += true; let n = 3; n += 'x'; return s + '|' + n; } plusEq()",
        want: "k12true|3x",
    },
    Case {
        what: "a getter and a data property added to Object.prototype under a name a string site reads",
        text: "function objproto() { function read(x) { 'use strict'; return x.added; } let out = ''; for (let i = 0; i < 60; i++) { if (i === 15) Object.defineProperty(Object.prototype, 'added', { configurable: true, get() { 'use strict'; return 'O' + typeof this + this; } }); out = String(read('xy')); } delete Object.prototype.added; function data(x) { return x.flag; } let flag = ''; for (let i = 0; i < 60; i++) { if (i === 15) Object.prototype.flag = 'P'; flag = String(data('xy')); } delete Object.prototype.flag; return out + ':' + flag; } objproto()",
        want: "Ostringxy:P",
    },
    Case {
        what: "a view stays intact while its buffer grows and other strings are allocated",
        text: "function viewLive() { let s = 'v'.repeat(200); const view = s.slice(10, 80); for (let i = 0; i < 400; i++) s += 'w'; let junk = ''; for (let i = 0; i < 300; i++) junk += 'j'; return view.length + ':' + view.slice(0, 4) + ':' + view.slice(-4) + ':' + s.length + ':' + s.slice(-2) + ':' + junk.length; } viewLive()",
        want: "70:vvvv:vvvv:600:ww:300",
    },
    Case {
        what: "a lone-surrogate literal and a pair written as one literal",
        text: "const s = '\\uD800'; const p = '\\uD800\\uDE00'; function f() { let n = 0; for (let i = 0; i < 40; i++) n += s.length + p.length; return [s.length, s.charCodeAt(0), p.length, p.charCodeAt(0), p.charCodeAt(1), [...p].length, n].join(':'); } f()",
        want: "1:55296:2:55296:56832:1:120",
    },
    Case {
        what: "a method found behind String.prototype after String.prototype loses its prototype, inside a function",
        text: "function f(s) { return s.hasOwnProperty; } let n = 0; for (let i = 0; i < 20000; i++) { if (i === 10000) Object.setPrototypeOf(String.prototype, null); n += (f('abc') === undefined) ? 1 : 2; } n",
        want: "30000",
    },
    Case {
        what: "the same at the top level",
        text: "let n = 0; const s = 'abc'; for (let i = 0; i < 20000; i++) { if (i === 10000) Object.setPrototypeOf(String.prototype, null); n += (s.hasOwnProperty === undefined) ? 1 : 2; } n",
        want: "30000",
    },
    Case {
        what: "String.prototype gets another prototype half-way through a compiled loop",
        text: "function f(s) { return s.toLocaleString === undefined ? 0 : typeof s.isPrototypeOf; } let out = ''; for (let i = 0; i < 20000; i++) { if (i === 10000) Object.setPrototypeOf(String.prototype, { isPrototypeOf: 7, extra: 1 }); const r = f('abc'); if (i === 9999 || i === 10000 || i === 19999) out += r + ','; } out + typeof 'x'.extra",
        want: "function,number,number,number",
    },
    Case {
        what: "valueOf on an object operand moves a global between two reads in one iteration",
        text: "var g = 1; let s = ''; let acc = 0; const o = { valueOf() { g += 1; return 'v'; } }; for (let i = 0; i < 20000; i++) { const a = g; s = 'k' + i; const t = (i % 100 === 99) ? ('x' + o) : ('x' + i); acc += g - a; acc += t.length & 1; } acc + ':' + g + ':' + s",
        want: "9199:201:k19999",
    },
    Case {
        what: "Symbol.toPrimitive writes a global and a property of the global object",
        text: "var h = 0; let acc = 0; const o = { [Symbol.toPrimitive]() { h = h + 2; globalThis.z = h; return 5; } }; function run(n) { let t = 0; for (let i = 0; i < n; i++) { const before = h; const r = (i & 1023) === 7 ? o + 'y' : i + 'y'; t += (h - before) + r.length; } return t; } acc = run(30000); acc + ':' + h + ':' + z",
        want: "168843:60:60",
    },
];

fn completion(text: &str, tier: Tier) -> String {
    let platform = DefaultPlatform::default();
    let mut isolate = Isolate::new(&platform, CreateParams::default());
    let mut executor = match tier {
        Tier::Interpreter => Executor::default(),
        Tier::Immediate => Executor::default().with_tier(j8jit::Jit::from(0)),
        Tier::Default => Executor::default().with_tier(j8jit::Jit::default()),
    };
    let cx = isolate.enter(|scope| scope.persist(scope.context(&Builtins)));
    let r = isolate.run(&mut executor, &cx, &Source { text, file: "string_fast.js" }).unwrap_or_else(|e| panic!("script failed ({}): {e:?}\n{text}", tier.label()));
    isolate.enter(|scope| {
        let realm = scope.context_of(&cx);
        let mut a = scope.agent(realm, &executor);
        let v = a.local(&r);
        String::from(&**a.to_string(v).unwrap_or_else(|e| panic!("ToString failed ({}): {e:?}\n{text}", tier.label())))
    })
}

fn run_cases() {
    for case in CASES {
        for tier in [Tier::Interpreter, Tier::Immediate, Tier::Default] {
            assert_eq!(completion(case.text, tier), case.want, "{} ({})", case.what, tier.label());
        }
    }
}

#[test]
fn string_paths_match_the_interpreter() {
    let ran = std::thread::Builder::new().stack_size(16 << 20).spawn(run_cases).expect("a thread with room for the compiled frames");
    ran.join().expect("the cases finish");
}

j8::runtime! {
    Bare {}
}

#[test]
fn the_same_constant_in_two_realms() {
    let platform = Bare::platform(DefaultPlatform::default());
    j8::EventLoopSet::scope(&platform, std::num::NonZeroUsize::MIN, |set| {
        let a = Bare::runtime(set, Executor::default()).expect("realm a");
        let b = Bare::runtime(set, Executor::default()).expect("realm b");
        let script = "function word() { return 'hello-world'; } const m = new Map(); m.set(word(), 1); const s = '\\uD800'; [word() === 'hello-world', m.get(word()), s.length, s.charCodeAt(0)].join(':')";
        let va = a.eval(script, "a.js").expect("realm a script");
        let vb = b.eval(script, "b.js").expect("realm b script");
        assert_eq!(a.text(&va), "true:1:1:55296");
        assert_eq!(b.text(&vb), "true:1:1:55296");
    });
}
