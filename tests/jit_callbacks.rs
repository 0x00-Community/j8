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
        what: "a site warmed with arrays reads through a null prototype",
        text: "function f(o) { return o.push ? 1 : 2; } const a = []; let n = 0; for (let i = 0; i < 5000; i++) n += f(a); n += f(Object.create(null)); n",
        want: "5002",
    },
    Case {
        what: "map, filter and reduce with compiled callbacks",
        text: "const a = []; for (let i = 0; i < 3000; i++) a.push(i); a.map((x) => x * 3).filter((x) => x % 2 === 0).reduce((s, x) => s + x, 0)",
        want: "6745500",
    },
    Case {
        what: "a comparator called from sort",
        text: "const a = []; let seed = 7; for (let i = 0; i < 3000; i++) { seed = (seed * 1103515245 + 12345) % 2147483648; a.push(seed); } a.sort((x, y) => x - y); let ok = 1; for (let i = 1; i < a.length; i++) if (a[i - 1] > a[i]) ok = 0; ok * 1000000000000 + a[0] + a[a.length - 1]",
        want: "1002147150080",
    },
    Case {
        what: "a callback that uses all three arguments",
        text: "const a = []; for (let i = 0; i < 2000; i++) a.push(i * 2); a.map((x, i, all) => x + i + all.length).reduce((s, x) => s + x, 0)",
        want: "9997000",
    },
    Case {
        what: "forEach writing to a captured variable",
        text: "const a = []; for (let i = 0; i < 2000; i++) a.push(i); let s = 0; a.forEach((x) => { s += x * x; }); s",
        want: "2664667000",
    },
    Case {
        what: "some, every, find, findIndex, findLast and findLastIndex",
        text: "const a = []; for (let i = 0; i < 2000; i++) a.push(i); let s = 0; for (let r = 0; r < 20; r++) { s += a.some((x) => x === 1500) ? 1 : 0; s += a.every((x) => x < 1999) ? 1 : 0; s += a.find((x) => x > 1000); s += a.findIndex((x) => x === 77); s += a.findLast((x) => x % 7 === 0); s += a.findLastIndex((x) => x < 10); } s",
        want: "61660",
    },
    Case {
        what: "an exception thrown inside a compiled callback reaches the caller of the builtin",
        text: "const a = []; for (let i = 0; i < 3000; i++) a.push(i); let seen = 0; let r = ''; try { a.forEach((x) => { seen++; if (x === 2500) throw new RangeError('at ' + x); }); } catch (e) { r = e.name + ':' + e.message; } r + ':' + seen",
        want: "RangeError:at 2500:2501",
    },
    Case {
        what: "thisArg reaches a compiled callback",
        text: "const a = []; for (let i = 0; i < 2000; i++) a.push(i); a.map(function (x) { return x + this.k; }, { k: 7 }).reduce((s, x) => s + x, 0)",
        want: "2013000",
    },
    Case {
        what: "reduceRight and a callback that calls a builtin with its own callback",
        text: "const a = []; for (let i = 0; i < 300; i++) a.push(i); a.reduceRight((s, x) => s + [x, x + 1].map((y) => y * 2).reduce((p, q) => p + q, 0), 0)",
        want: "180000",
    },
    Case {
        what: "a callback with four parameters and one with a rest parameter",
        text: "const a = []; for (let i = 0; i < 2000; i++) a.push(i); a.map((x, i, all, extra) => x + (extra === undefined ? 1 : 0)).reduce((s, x) => s + x, 0) + a.map((...p) => p.length).reduce((s, x) => s + x, 0)",
        want: "2007000",
    },
    Case {
        what: "a callback that returns doubles, strings and objects",
        text: "const a = []; for (let i = 0; i < 2000; i++) a.push(i); const d = a.map((x) => x / 4); const t = a.map((x) => 'n' + x); const o = a.map((x) => ({ v: x })); d[1999] + ':' + t[1999] + ':' + o[1999].v + ':' + d.length",
        want: "499.75:n1999:1999:2000",
    },
    Case {
        what: "Map and Set forEach with compiled callbacks",
        text: "const m = new Map(); const s = new Set(); for (let i = 0; i < 2000; i++) { m.set(i, i * 2); s.add(i * 3); } let t = 0; m.forEach((v, k) => { t += v - k; }); s.forEach((v) => { t += v; }); t",
        want: "7996000",
    },
    Case {
        what: "forEach visits elements pushed and popped during the walk",
        text: "const a = [1, 2, 3, 4, 5]; const p = []; a.forEach((x) => { p.push(x); if (x === 2) a.push(9); }); const b = [1, 2, 3, 4, 5]; const q = []; b.forEach((x) => { q.push(x); if (x === 2) b.pop(); }); p.join(',') + '|' + q.join(',')",
        want: "1,2,3,4,5|1,2,3,4",
    },
    Case {
        what: "forEach stops visiting after length is set to zero",
        text: "const a = [1, 2, 3, 4, 5]; const s = []; a.forEach((x) => { s.push(x); if (x === 1) a.length = 0; }); s.join(',') + ':' + a.length",
        want: "1:0",
    },
    Case {
        what: "forEach skips an element deleted ahead and sees a replacement",
        text: "const a = [1, 2, 3, 4, 5]; const d = []; a.forEach((x, i) => { d.push(i + ':' + x); if (i === 1) delete a[2]; }); const b = [1, 2, 3, 4, 5]; const r = []; b.forEach((x, i) => { r.push(x); if (i === 1) b[3] = 40; if (i === 3) b[0] = 10; }); d.join(',') + '|' + r.join(',') + ':' + b.join(',')",
        want: "0:1,1:2,3:4,4:5|1,2,3,40,5:10,2,3,40,5",
    },
    Case {
        what: "a getter between callbacks changes a global the compiled callback reads",
        text: "let g = 1; const a = []; for (let i = 0; i < 20; i++) a.push(i); function f() { return g; } for (let r = 0; r < 5; r++) a.forEach(f); g = 1; Object.defineProperty(a, '5', { get() { g = 4; return 50; }, configurable: true }); const seen = []; a.forEach(() => seen.push(g)); seen.join(',')",
        want: "1,1,1,1,1,4,4,4,4,4,4,4,4,4,4,4,4,4,4,4",
    },
    Case {
        what: "a callback compiled on numbers keeps working when later arguments are strings",
        text: "function d(x) { return x + x; } const w = []; for (let i = 0; i < 80; i++) w.push(i); w.map(d); const a = [1, 2, 3, 4]; for (let i = 0; i < 4; i++) a.push('s' + i); const m = a.map(d); m[0] + ':' + m[4] + ':' + m.length",
        want: "2:s0s0:8",
    },
    Case {
        what: "a callback that throws on the first element and one that throws on the last",
        text: "const a = [1, 2, 3, 4]; let n = 0; let e1 = ''; try { a.forEach((x) => { n++; if (x === 1) throw new Error('first'); }); } catch (e) { e1 = e.message; } let m = 0; let e2 = ''; try { a.map((x) => { m++; if (x === 4) throw new Error('last'); return x; }); } catch (e) { e2 = e.message; } e1 + ':' + n + ':' + e2 + ':' + m",
        want: "first:1:last:4",
    },
    Case {
        what: "deep recursion through map ends in a catchable RangeError",
        text: "function f(n) { return [0].map(() => f(n + 1))[0]; } let r = ''; try { f(0); r = 'no'; } catch (e) { r = e instanceof RangeError ? 'range' : e.name; } r",
        want: "range",
    },
    Case {
        what: "holes, a sparse array and an array-like",
        text: "const m = [1, , 3].map((x) => x + 1); const holes = m.length + ':' + (0 in m) + ':' + (1 in m) + ':' + m[0] + ':' + m[2]; const sp = []; sp[0] = 1; sp[100] = 2; const s = []; sp.forEach((x, i) => s.push(i + ':' + x)); const like = Array.prototype.map.call({ length: 2, 0: 'a', 1: 'b' }, (x) => x + x).join(','); const packed = [1, , 3].filter((x) => true).join(','); holes + '|' + s.join(',') + '|' + like + '|' + packed",
        want: "3:true:false:2:4|0:1,100:2|aa,bb|1,3",
    },
    Case {
        what: "a subclass of Array keeps its species for map and filter",
        text: "class C extends Array {} const a = new C(1, 2, 3); const m = a.map((x) => x + 1); const f = a.filter((x) => x > 1); class D extends Array { static get [Symbol.species]() { return Array; } } const d = new D(1, 2, 3).map((x) => x * 2); (m instanceof C) + ':' + m.join(',') + ':' + (f instanceof C) + ':' + f.join(',') + '|' + (d instanceof D) + ':' + (d instanceof Array) + ':' + d.join(',')",
        want: "true:2,3,4:true:2,3|false:true:2,4,6",
    },
    Case {
        what: "Map and Set forEach observe deletions and additions during the walk",
        text: "const m = new Map(); m.set('a', 1); m.set('b', 2); m.set('c', 3); const seen = []; m.forEach((v, k, map) => { seen.push(k + v); if (k === 'a') { map.delete('b'); map.set('d', 4); } }); const t = new Set([1, 2, 3]); const ss = []; t.forEach((v, _, set) => { ss.push(v); if (v === 1) { set.delete(2); set.add(4); } }); seen.join(',') + '|' + ss.join(',')",
        want: "a1,c3,d4|1,3,4",
    },
    Case {
        what: "the same function used in nested and recursive builtins",
        text: "function f(x) { if (x === 0) return [1, 2].map(f).join(','); return x + 1; } function fact(n) { return n <= 0 ? 1 : [n].map((x) => x * fact(x - 1))[0]; } function g(x) { return [x].map(g).length ? x : x; } let r = ''; try { r = String([10, 11, 12].map(g)); } catch (e) { r = e.name; } f(0) + '|' + fact(6) + '|' + r",
        want: "2,3|720|RangeError",
    },
    Case {
        what: "a getter calls map with the same function the outer map is using",
        text: "function f(x) { return x + 1; } const warm = []; for (let i = 0; i < 40; i++) warm.push(i); warm.map(f); const a = [1, 2, 3, 4]; Object.defineProperty(a, '1', { get() { return [10, 20].map(f).join(':'); }, configurable: true }); a.map(f).join(',')",
        want: "2,11:211,4,5",
    },
    Case {
        what: "a comparator result's valueOf sorts another array with the same comparator",
        text: "let extra = 0; function cmp(x, y) { extra++; return (x > y) - (x < y); } const box = { valueOf() { [1, 3, 2].sort(cmp); return 5; } }; const a = [box, 1]; const before = extra; a.sort(cmp); (extra - before) + ':' + a.map((x) => typeof x === 'object' ? 'box' : x).join(',')",
        want: "7:1,box",
    },
    Case {
        what: "callbacks that observe arguments, thisArg and a rest parameter",
        text: "const n = [1, 2].map(function () { return arguments.length; }).join(',') + ':' + [1, 2].reduce(function () { return arguments.length; }, 0) + ':' + [1].map((...p) => p.length)[0]; const t = [10].map(function () { return typeof this + ':' + this.valueOf(); }, 5)[0] + '|' + [10].map(function () { 'use strict'; return typeof this + ':' + this; }, 5)[0] + '|' + [1, 2].map(function (x) { return x + this.k; }, { k: 3 }).join(','); n + '|' + t",
        want: "3,3:4:3|object:5|number:5|4,5",
    },
    Case {
        what: "a proxy, a frozen array and reduce that deletes an element",
        text: "const a = [1, 2, 3]; const p = new Proxy(a, { get(t, k, r) { return Reflect.get(t, k, r); } }); const frozen = Object.freeze([1, 2, 3]); const b = [1, 2, 3, 4]; const n = b.reduce((s, x, i) => { if (i === 0) delete b[2]; return s + x; }, 0); p.map((x) => x + 1).join(',') + '|' + frozen.map((x) => x + 1).join(',') + '|' + frozen.forEach(() => 1) + '|' + n",
        want: "2,3,4|2,3,4|undefined|7",
    },
    Case {
        what: "sort keeps undefined and holes in place relative to numbers and stays stable",
        text: "const a = [3, undefined, 1, , 2]; a.sort((x, y) => x - y); const s = [{ k: 1, i: 0 }, { k: 2, i: 1 }, { k: 1, i: 2 }]; s.sort((p, q) => p.k - q.k); a.join(',') + ':' + a.length + ':' + (3 in a) + ':' + (4 in a) + '|' + s.map((p) => p.i).join(',')",
        want: "1,2,3,,:5:true:false|0,2,1",
    },
    Case {
        what: "a long callback loop allocates while the callback is alive",
        text: "const a = []; for (let i = 0; i < 120000; i++) a.push(i); let s = 0; a.forEach((x) => { s += x; if ((x & 1023) === 0) { const o = { v: x }; s += o.v - x; } }); s",
        want: "7199940000",
    },
    Case {
        what: "a getter on the array reassigns the globals the callback reads",
        text: "var g = 1; let h = 10; globalThis.q = 100; const a = []; for (let i = 0; i < 300; i++) a.push(i); Object.defineProperty(a, 100, { get() { g = 50; h = 500; globalThis.q = 5000; return 100; }, configurable: true, enumerable: true }); a.map((x) => g + h + q).reduce((s, x) => s + x, 0) + ':' + g + h + q",
        want: "1121100:505005000",
    },
    Case {
        what: "a bound native used as a getter deletes a global between two callbacks",
        text: "globalThis.zz = 3; const a = []; for (let i = 0; i < 300; i++) a.push(i); Object.defineProperty(a, 130, { get: Reflect.deleteProperty.bind(null, globalThis, 'zz'), configurable: true, enumerable: true }); let r; try { r = a.map((x) => zz).length; } catch (e) { r = e.name; } r",
        want: "ReferenceError",
    },
    Case {
        what: "a bound native used as a getter turns a global into an accessor between two callbacks",
        text: "globalThis.acc = 1; const a = []; for (let i = 0; i < 300; i++) a.push(i); Object.defineProperty(a, 120, { get: Object.defineProperty.bind(null, globalThis, 'acc', { get() { return 1000; }, configurable: true }), configurable: true, enumerable: true }); a.map((x) => acc).reduce((s, x) => s + (typeof x === 'number' ? x : 0), 0)",
        want: "180120",
    },
    Case {
        what: "a bound native used as a getter writes a global between two callbacks",
        text: "globalThis.w1 = 7; const a = []; for (let i = 0; i < 300; i++) a.push(i); Object.defineProperty(a, 140, { get: Reflect.set.bind(null, globalThis, 'w1', 70), configurable: true, enumerable: true }); a.map((x) => w1).reduce((s, x) => s + x, 0)",
        want: "12180",
    },
    Case {
        what: "map into the global object through a species constructor while the callback counts in a global",
        text: "globalThis.cnt = 0; class G extends Array { static get [Symbol.species]() { return function () { return globalThis; }; } } const a = new G(); for (let i = 0; i < 300; i++) a.push(i); const r = a.map((x) => { cnt += 1; return x; }); let s = 0; for (let i = 0; i < 300; i++) { s += globalThis[i]; delete globalThis[i]; } (r === globalThis) + ':' + cnt + ':' + s",
        want: "true:300:44850",
    },
    Case {
        what: "filter into the global object through a species constructor",
        text: "globalThis.cnt2 = 0; class G extends Array { static get [Symbol.species]() { return function () { return globalThis; }; } } const a = new G(); for (let i = 0; i < 300; i++) a.push(i); const r = a.filter((x) => { cnt2 += 2; return x % 3 === 0; }); let s = 0; for (let i = 0; i < 100; i++) { s += globalThis[i]; delete globalThis[i]; } (r === globalThis) + ':' + cnt2 + ':' + s",
        want: "true:600:14850",
    },
    Case {
        what: "a comparator result whose valueOf is a bound native that writes a global",
        text: "globalThis.v1 = 1; const a = []; for (let i = 0; i < 300; i++) a.push(i); let n = 0; a.sort((x, y) => { n += v1; return { valueOf: Reflect.set.bind(null, globalThis, 'v1', 2) }; }); (n > 300) + ':' + v1 + ':' + a.length",
        want: "true:2:300",
    },
    Case {
        what: "Map and Set forEach while the callback changes the global it reads",
        text: "var k = 5; let s = 0; const st = new Set(); const m = new Map(); for (let i = 0; i < 300; i++) { st.add(i); m.set(i, i); } st.forEach((v) => { s += k; if (v === 10) k = 6; }); m.forEach((v) => { s += k; if (v === 10) k = 7; }); s",
        want: "3878",
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
    let r = isolate.run(&mut executor, &cx, &Source { text, file: "jit_callbacks.js" }).unwrap_or_else(|e| panic!("script failed ({}): {e:?}\n{text}", tier.label()));
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
            assert_eq!(completion(case.text, tier), case.want, "{} ({}): {}", case.what, tier.label(), case.text);
        }
    }
}

#[test]
fn builtins_calling_compiled_code_match_the_interpreter() {
    let ran = std::thread::Builder::new().stack_size(if cfg!(debug_assertions) { 64 << 20 } else { 16 << 20 }).spawn(run_cases).expect("a thread with room for callback recursion");
    ran.join().expect("the cases finish");
}
