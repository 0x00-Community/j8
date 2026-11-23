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
        what: "push of integers, doubles, strings and objects across element-store growth",
        text: "function f() { const a = []; let s = 0; for (let i = 0; i < 16385; i++) s += a.push(i % 7); const b = []; for (let i = 0; i < 200; i++) b.push(i * 0.5); const c = []; for (let i = 0; i < 200; i++) c.push('s' + i); const d = []; for (let i = 0; i < 50; i++) d.push({ v: i }); const e = [1, , 3]; const hp = e.push(4); const z = [9, 8]; z.length = 0; const zp = z.push(5); return a.length + ':' + a[127] + ':' + a[128] + ':' + a[129] + ':' + a[16383] + ':' + a[16384] + ':' + s + ':' + b[1] + ':' + c[199] + ':' + d[49].v + ':' + hp + ':' + e.length + ':' + (1 in e) + ':' + e.join(',') + ':' + zp + ':' + z.length + ':' + z[0]; } f()",
        want: "16385:1:2:3:3:4:134242305:0.5:s199:49:4:4:false:1,,3,4:1:1:5",
    },
    Case {
        what: "an indexed setter on Array.prototype keeps push on the generic path",
        text: "function f() { let sets = 0; Object.defineProperty(Array.prototype, '0', { set() { sets++; }, configurable: true }); const b = []; let n = 0; for (let i = 0; i < 4000; i++) n += b.push(i); const r = sets + ':' + n + ':' + b.length + ':' + String(b[0]) + ':' + String(b[1]); delete Array.prototype[0]; return r; } f()",
        want: "1:8002000:4000:undefined:1",
    },
    Case {
        what: "replacing Array.prototype.push halfway runs the new function on the next call",
        text: "function f(n) { const a = []; let s = 0; for (let i = 0; i < n; i++) s += a.push(i); const saved = Array.prototype.push; Array.prototype.push = function () { return 1000; }; for (let i = 0; i < 100; i++) s += a.push(i); Array.prototype.push = saved; return s + ':' + a.length; } f(5000)",
        want: "12602500:5000",
    },
    Case {
        what: "replacing Array.prototype.push inside the compiled loop is seen on the next push",
        text: "function f() { const a = []; let s = 0; const saved = Array.prototype.push; for (let i = 0; i < 8000; i++) { if (i === 4000) Array.prototype.push = function () { return 7; }; s += a.push(i); } Array.prototype.push = saved; return s + ':' + a.length; } f()",
        want: "8030000:4000",
    },
    Case {
        what: "an own push installed inside the compiled loop shadows the prototype method",
        text: "function f() { const a = []; let s = 0; for (let i = 0; i < 8000; i++) { if (i === 4000) a.push = function () { return 4; }; s += a.push(i); } return s + ':' + a.length + ':' + a[3999]; } f()",
        want: "8018000:4000:3999",
    },
    Case {
        what: "an own push on one array shadows the prototype method",
        text: "function f() { const a = []; let s = 0; for (let i = 0; i < 5000; i++) s += a.push(i); a.push = function () { return 3; }; for (let i = 0; i < 10; i++) s += a.push(i); return s + ':' + a.length + ':' + a[4999]; } f()",
        want: "12502530:5000:4999",
    },
    Case {
        what: "one call site switches among an array, a subclass, an array-like, a frozen array and a proxy",
        text: "function f() { const a = []; class C extends Array {} const c = new C(); const like = { length: 0 }; Object.setPrototypeOf(like, Array.prototype); const frozen = Object.freeze([1]); const p = new Proxy([1, 2], {}); const recvs = [a, c, like, frozen, p]; let s = 0; let thrown = 0; function step(recv, x) { return recv.push(x); } for (let i = 0; i < 6000; i++) { try { s += step(recvs[i % 5], i); } catch (e) { thrown++; } } return s + ':' + a.length + ':' + c.length + ':' + like.length + ':' + frozen.length + ':' + p.length + ':' + thrown; } f()",
        want: "2884800:1200:1200:1200:1:1202:1200",
    },
    Case {
        what: "push.call on undefined, null and an array-like keeps today's messages and result",
        text: "function f() { let r = ''; try { Array.prototype.push.call(undefined, 1); } catch (e) { r = e.name + ':' + e.message; } try { Array.prototype.push.call(null, 1); } catch (e) { r += '|' + e.name + ':' + e.message; } const like = { length: 1, 0: 'z' }; const n = Array.prototype.push.call(like, 'q'); return r + '|' + n + ':' + like.length + ':' + like[0] + ':' + like[1]; } f()",
        want: "TypeError:Cannot convert undefined or null to object|TypeError:Cannot convert undefined or null to object|2:2:z:q",
    },
    Case {
        what: "frozen, sealed, non-extensible and read-only length arrays throw as today",
        text: "function f() { const a = Object.freeze([1]); let r = ''; for (let i = 0; i < 100; i++) { try { a.push(2); } catch (e) { r = e.name + ':' + e.message; } } const b = Object.seal([1]); try { b.push(2); } catch (e) { r += '|' + e.message; } const c = [1]; Object.preventExtensions(c); try { c.push(2); } catch (e) { r += '|' + e.message; } const d = [1]; Object.defineProperty(d, 'length', { writable: false }); try { d.push(2); } catch (e) { r += '|' + e.message; } const e = [1, 2]; Object.defineProperty(e, 'length', { writable: false }); try { e.pop(); } catch (e2) { r += '|' + e2.message; } const fr = Object.freeze([1, 2, 3]); try { fr.pop(); } catch (e2) { r += '|' + e2.message; } return r + ':' + a.length + ':' + d.length + ':' + e.length + ':' + fr.length; } f()",
        want: "TypeError:Cannot add property 1, object is not extensible|Cannot add property 1, object is not extensible|Cannot add property 1, object is not extensible|Cannot assign to read only property '1'|Cannot assign to read only property 'length'|Cannot delete property '2' of [object Array]:1:1:2:3",
    },
    Case {
        what: "a push that reallocates is visible to a built-in iterating the same array",
        text: "function f() { const a = []; for (let i = 0; i < 100; i++) a.push(i); const seen = []; a.forEach((x) => { seen.push(x); if (x === 50) { for (let k = 0; k < 200; k++) a.push(1000 + k); } }); return seen.length + ':' + seen[0] + ':' + seen[99] + ':' + a.length + ':' + a[100] + ':' + a[299]; } f()",
        want: "100:0:99:300:1000:1199",
    },
    Case {
        what: "pop and a two-argument push",
        text: "function fill(n) { const a = []; for (let i = 0; i < n; i++) a.push(i * 0.5); return a; } let s = 0; for (let r = 0; r < 3; r++) { const a = fill(100); while (a.length > 50) s += a.pop(); s += a.push('x', 'y') + a.length; } const empty = []; let last = ''; for (let i = 0; i < 50; i++) last = String(empty.pop()); empty.push(1, 2); s + ':' + last + ':' + empty.pop() + ':' + empty.length",
        want: "5899.5:undefined:2:1",
    },
    Case {
        what: "Map and Set key equality, subclasses, WeakMap and deletion during forEach",
        text: "function f() { const m = new Map(); m.set(-0, 'neg'); const z = m.get(0) + ':' + m.get(-0) + ':' + m.has(0) + ':' + m.size; m.set(NaN, 'nan'); const nan = m.get(NaN) + ':' + m.has(NaN); m.set(1.5, 'f'); m.set(2 ** 53, 'big'); m.set('a' + 'b', 7); const o = {}; m.set(o, 9); const sym = Symbol('s'); m.set(sym, 3); m.set('你' + '好', 2); let s = 0; for (let i = 0; i < 1000; i++) { m.set(i, i + 1); s += m.get(i % 1000); } const st = new Set(); st.add(-0); st.add(NaN); st.add('x' + 'y'); st.add('你' + '好'); for (let i = 0; i < 1000; i++) st.add(i * 3); let hit = 0; for (let i = 0; i < 3000; i++) if (st.has(i)) hit++; st.delete(NaN); st.add(NaN); class M extends Map {} const mm = new M(); mm.set(1, 2); const w = new WeakMap(); let wm = ''; try { Map.prototype.get.call(w, o); } catch (e) { wm = e.name + ':' + e.message; } let bad = ''; try { Map.prototype.get.call(st, 1); } catch (e) { bad = e.message; } try { Set.prototype.has.call(new Map(), 1); } catch (e) { bad += '|' + e.message; } m.delete(5); m.set(5, 50); let order = ''; const md = new Map(); for (let i = 0; i < 20; i++) md.set(i, i); md.forEach((v, k) => { order += k + ','; if (k === 5) md.delete(6); md.delete(k); }); return z + ':' + nan + ':' + m.get(1.5) + ':' + m.get(2 ** 53) + ':' + m.get(2 ** 53 + 1) + ':' + m.get('ab') + ':' + m.get(o) + ':' + m.get({}) + ':' + m.get(sym) + ':' + m.get('你好') + ':' + s + ':' + st.has(0) + ':' + st.has(NaN) + ':' + st.has('xy') + ':' + st.has('你好') + ':' + hit + ':' + mm.get(1) + ':' + (mm instanceof M) + ':' + wm + ':' + bad + ':' + m.get(5) + ':' + m.size + ':' + order + ':' + md.size; } f()",
        want: "neg:neg:true:1:nan:true:f:big:big:7:9:undefined:3:2:500500:true:true:true:true:1000:2:true:TypeError:Method called on an incompatible receiver:Method called on an incompatible receiver|Method called on an incompatible receiver:50:1007:0,1,2,3,4,5,7,8,9,10,11,12,13,14,15,16,17,18,19,:0",
    },
    Case {
        what: "String.fromCharCode over the uint16 range and with a valueOf argument",
        text: "function f() { let s = 0; for (let i = 0; i <= 70000; i++) s += String.fromCharCode(i).charCodeAt(0); let n = 0; const o = { valueOf() { n = 9; return 67; } }; return s + ':' + String.fromCharCode(o) + ':' + n + ':' + String.fromCharCode().length + ':' + String.fromCharCode(65, 66) + ':' + String.fromCharCode(-1).charCodeAt(0) + ':' + String.fromCharCode(NaN).charCodeAt(0) + ':' + String.fromCharCode(Infinity).charCodeAt(0) + ':' + String.fromCharCode(70000).charCodeAt(0) + ':' + String.fromCharCode('65'); } f()",
        want: "2176286008:C:9:0:AB:65535:0:0:4464:A",
    },
    Case {
        what: "one site alternates between two natives",
        text: "function f() { const arr = []; const map = new Map(); map.push = Map.prototype.set; function step(recv, x) { return recv.push(x, x); } let n = 0; for (let i = 0; i < 200; i++) { const r = step(i % 2 ? map : arr, i); n += typeof r === 'number' ? r : 0; } return n + ':' + arr.length + ':' + map.size + ':' + map.get(1); } f()",
        want: "10100:200:100:1",
    },
    Case {
        what: "a top-level loop reads a global that the generic fallback writes",
        text: "var g = 1; let s = 0; const a = []; const like = { length: 0 }; Object.setPrototypeOf(like, Array.prototype); Object.defineProperty(like, '0', { configurable: true, set() { g = 8; } }); let useLike = false; for (let i = 0; i < 4000; i++) { if (i === 2000) useLike = true; s += g; (useLike ? like : a).push(1); } s + ':' + g + ':' + a.length + ':' + like.length",
        want: "17993:8:2000:2000",
    },
    Case {
        what: "a let written by the generic fallback is visible on the next iteration",
        text: "function run() { let g = 1; let s = 0; const a = []; const like = { length: 0 }; Object.setPrototypeOf(like, Array.prototype); Object.defineProperty(like, '0', { configurable: true, set() { g = 8; } }); let useLike = false; for (let i = 0; i < 4000; i++) { if (i === 2000) useLike = true; s += g; (useLike ? like : a).push(1); } return s + ':' + g; } run()",
        want: "17993:8",
    },
    Case {
        what: "fromCharCode's valueOf writes a global the loop reads",
        text: "var g = 1; function run() { let s = 0; const o = { valueOf() { g = 4; return 65; } }; for (let i = 0; i < 4000; i++) { s += g; String.fromCharCode(i < 2000 ? 65 : o); } return s + ':' + g; } run()",
        want: "9997:4",
    },
    Case {
        what: "Math abs, floor, ceil, sqrt, min and max on numbers, and valueOf otherwise",
        text: "function f() { let s = 0; for (let i = 0; i < 3000; i++) s += Math.abs(i - 1500) + Math.floor(i / 7) + Math.ceil(i / 7) + Math.sqrt(i) + Math.max(i, 1000) + Math.min(i, 99); let n = 0; const o = { valueOf() { n++; return -4; } }; const r = Math.abs(o) + Math.floor(o) + Math.min(o, 1) + Math.max(2, o); let amin = 1, amax = 1, nan = 1; for (let i = 0; i < 100; i++) { amin = 1 / Math.min(-0, 0); amax = 1 / Math.max(-0, 0); nan = Math.min(nan, NaN); } return s + ':' + r + ':' + n + ':' + amin + ':' + amax + ':' + (1 / Math.abs(-0)) + ':' + String(nan); } f()",
        want: "8935853.918247666:-2:4:-Infinity:Infinity:Infinity:NaN",
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
    let r = isolate.run(&mut executor, &cx, &Source { text, file: "jit_intrinsics.js" }).unwrap_or_else(|e| panic!("script failed ({}): {e:?}\n{text}", tier.label()));
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
fn compiled_calls_to_builtins_match_the_interpreter() {
    let ran = std::thread::Builder::new().stack_size(16 << 20).spawn(run_cases).expect("a thread with room for the compiled frames");
    ran.join().expect("the cases finish");
}
