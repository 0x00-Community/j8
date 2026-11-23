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
        what: "each evaluation of a regexp literal is a distinct object",
        text: "function f() { let bad = 0; let src = ''; let flags = ''; let li = -1; for (let i = 0; i < 4000; i++) { const a = /a/; const b = /a/; if (a === b) bad++; src = a.source; flags = a.flags; li = a.lastIndex; } return bad + ':' + src + ':' + flags + ':' + li; } f()",
        want: "0:a::0",
    },
    Case {
        what: "a kept global regexp advances lastIndex and a fresh literal does not",
        text: "function f() { const re = /a/g; let n = 0; for (let i = 0; i < 4000; i++) if (re.exec('ab')) n++; let fresh = 0; for (let i = 0; i < 100; i++) if (/a/g.exec('ab')) fresh++; return n + ':' + re.lastIndex + ':' + fresh; } f()",
        want: "2000:0:100",
    },
    Case {
        what: "lastIndex and an own property written on one literal are absent on the next",
        text: "function f() { let bad = 0; for (let i = 0; i < 4000; i++) { const r = /a/; r.lastIndex = 5; r.x = i; const n = /a/; if (n.lastIndex !== 0 || n.x !== undefined) bad++; } return bad; } f()",
        want: "0",
    },
    Case {
        what: "source and every flag, including combinations of d, g, i, m, s, u, v and y",
        text: "function f() { const rs = [/a/d, /a/g, /a/i, /a/m, /a/s, /a/u, /a/v, /a/y, /a/dg, /a/gi, /a/im, /a/ms, /a/su, /a/vy, /a/dgsy]; const out = []; for (let i = 0; i < 4000; i++) { const r = rs[i % rs.length]; out[i % rs.length] = r.source + '/' + r.flags + '/' + r.global + '/' + r.ignoreCase + '/' + r.multiline + '/' + r.dotAll + '/' + r.unicode + '/' + r.unicodeSets + '/' + r.sticky + '/' + r.hasIndices + '/' + r.lastIndex; } return out.join(';'); } f()",
        want: "a/d/false/false/false/false/false/false/false/true/0;a/g/true/false/false/false/false/false/false/false/0;a/i/false/true/false/false/false/false/false/false/0;a/m/false/false/true/false/false/false/false/false/0;a/s/false/false/false/true/false/false/false/false/0;a/u/false/false/false/false/true/false/false/false/0;a/v/false/false/false/false/false/true/false/false/0;a/y/false/false/false/false/false/false/true/false/0;a/dg/true/false/false/false/false/false/false/true/0;a/gi/true/true/false/false/false/false/false/false/0;a/im/false/true/true/false/false/false/false/false/0;a/ms/false/false/true/true/false/false/false/false/0;a/su/false/false/false/true/true/false/false/false/0;a/vy/false/false/false/false/false/true/true/false/0;a/dgsy/true/false/false/true/false/false/true/true/0",
    },
    Case {
        what: "a regexp kept alive still matches after thousands of other literals die",
        text: "function f() { const keep = /keep/gi; let n = 0; for (let i = 0; i < 20000; i++) { const r = /x/; if (r.source === 'x' && r !== keep) n++; } keep.lastIndex = 3; const before = keep.lastIndex; keep.lastIndex = 0; const m = keep.exec('zzkeep'); return keep.source + ':' + keep.flags + ':' + before + ':' + (m && m[0]) + ':' + keep.lastIndex + ':' + n + ':' + (keep === keep); } f()",
        want: "keep:gi:3:keep:6:20000:true",
    },
    Case {
        what: "replacing RegExp.prototype.test halfway runs the new function on that iteration",
        text: "function f() { let n = 0; for (let i = 0; i < 4000; i++) { if (i === 2000) RegExp.prototype.test = function () { return false; }; if (/\\d/.test('1')) n++; } return n; } f()",
        want: "2000",
    },
    Case {
        what: "replacing RegExp.prototype.test before the loop hides the built-in",
        text: "function f() { RegExp.prototype.test = function () { return false; }; let n = 0; for (let i = 0; i < 4000; i++) if (/\\d/.test('1')) n++; return n; } f()",
        want: "0",
    },
    Case {
        what: "replacing RegExp.prototype.exec halfway stops a kept global regexp",
        text: "function f() { let n = 0; const re = /a/g; for (let i = 0; i < 4000; i++) { if (i === 2000) RegExp.prototype.exec = function () { return null; }; if (re.exec('a')) n++; } return n + ':' + re.lastIndex; } f()",
        want: "1000:0",
    },
    Case {
        what: "a flags getter installed halfway is what the next literal reads",
        text: "function f() { let n = 0; let last = ''; for (let i = 0; i < 4000; i++) { if (i === 2000) Object.defineProperty(RegExp.prototype, 'flags', { get() { return 'X'; }, configurable: true }); last = /a/g.flags; if (last === 'g') n++; } return n + ':' + last; } f()",
        want: "2000:X",
    },
    Case {
        what: "a global getter installed halfway is what the next literal reads",
        text: "function f() { let n = 0; let last = ''; for (let i = 0; i < 4000; i++) { if (i === 2000) Object.defineProperty(RegExp.prototype, 'global', { get() { return 'G'; }, configurable: true }); last = String(/a/g.global); if (last === 'true') n++; } return n + ':' + last; } f()",
        want: "2000:G",
    },
    Case {
        what: "replacing Symbol.replace halfway changes the next literal replace",
        text: "function f() { let n = 0; let last = ''; for (let i = 0; i < 4000; i++) { if (i === 2000) RegExp.prototype[Symbol.replace] = function () { return 'Z'; }; last = 'o'.replace(/o/g, '0'); if (last === '0') n++; } return n + ':' + last; } f()",
        want: "2000:Z",
    },
    Case {
        what: "replacing String.prototype.replace halfway runs the new function on that iteration",
        text: "function f() { let n = 0; let last = ''; for (let i = 0; i < 4000; i++) { if (i === 2000) String.prototype.replace = function () { return 'Z'; }; last = 'o'.replace(/o/g, '0'); if (last === '0') n++; } return n + ':' + last; } f()",
        want: "2000:Z",
    },
    Case {
        what: "a regexp subclass keeps its instance, source, flags and lastIndex",
        text: "function f() { class R extends RegExp {} const kept = new R('a', 'g'); let n = 0; for (let i = 0; i < 4000; i++) { const r = new R('ab', 'i'); n += (r instanceof R) + (r instanceof RegExp) + (r.test('AB') ? 1 : 0) + (r.lastIndex === 0 ? 1 : 0) + (r.source === 'ab' ? 1 : 0); if (kept.exec('xa')) n++; } return n + ':' + (kept instanceof R) + ':' + kept.lastIndex + ':' + kept.source + ':' + kept.flags; } f()",
        want: "22000:true:0:a:g",
    },
    Case {
        what: "compile on a subclass throws and leaves the source",
        text: "function f() { class R extends RegExp {} const r = new R('a'); let msg = ''; for (let i = 0; i < 4000; i++) { try { r.compile('b'); msg = 'ok'; } catch (e) { msg = e.name + ':' + e.message; } } return msg + ':' + r.source; } f()",
        want: "TypeError:RegExp.prototype.compile cannot be used on RegExp subclass instances:a",
    },
    Case {
        what: "compile rewrites one literal and the next evaluation of the same literal stays /a/",
        text: "function f() { const kept = /a/; let bad = 0; for (let i = 0; i < 4000; i++) { const n = /a/; if (i === 2000) kept.compile('b'); if (n.source !== 'a' || n.flags !== '' || n === kept) bad++; } const other = /a/; return bad + ':' + kept.source + ':' + kept.flags + ':' + other.source + ':' + (kept !== other); } f()",
        want: "0:b::a:true",
    },
    Case {
        what: "the regexp.js loop counts matches and the length of the replacement",
        text: "function f() { const text = 'The quick brown fox 123 jumps over the lazy dog 4567 '; let c = 0; for (let i = 0; i < 4000; i++) { if (/\\d+/.test(text)) c++; c += text.replace(/o/g, '0').length; } return c; } f()",
        want: "216000",
    },
    Case {
        what: "test and replace agree on global, sticky, unicode, ignore-case, empty and failed matches",
        text: "function f() { let out = ''; for (let i = 0; i < 200; i++) { const g = /a/g; const y = /a/y; y.lastIndex = 1; const u = /\\u{1F600}/gu; const ign = /b/gi; out = [g.test('aba'), g.lastIndex, g.test('aba'), g.lastIndex, y.test('xa'), y.lastIndex, 'AbC'.replace(ign, 'x'), 'a\\nb'.replace(/^b/m, 'x'), 'ab'.replace(/(?:)/g, '-'), 'abc'.replace(/z/g, '0'), u.test('\\u{1F600}'), 'oo'.replace(/o/g, '0'), /a/y.test('xa')].join('|'); } return out; } f()",
        want: "true|1|true|3|true|2|AxC|a\nx|-a-b-|abc|true|00|false",
    },
    Case {
        what: "a replacement with $1, $& or $$ and a function replacement stay on the generic path",
        text: "function f() { let out = ''; for (let i = 0; i < 200; i++) { const r = /a/g; const seen = []; const rep = 'aba'.replace(r, function (m) { seen.push(r.lastIndex + m); return 'x'; }); out = ['ab'.replace(/(a)/, '[$1][$&][$$]'), 'z'.replace(/z/, '$$'), rep, seen.join(','), r.lastIndex].join('|'); } return out; } f()",
        want: "[a][a][$]b|$|xbx|0a,0a|0",
    },
    Case {
        what: "a non-string argument, a non-ascii subject and a lone surrogate go through the ordinary path",
        text: "function f() { const lead = String.fromCharCode(0xD800); const pair = String.fromCharCode(0xD800, 0xDC00); let out = ''; let order = ''; for (let i = 0; i < 80; i++) { const arg = { toString() { order = 's'; return 'aba'; } }; const r = /a/g; r.lastIndex = { valueOf() { order += ',li'; return 1; } }; const t = r.test(arg); out = [t, order, 'éé'.replace(/é/g, 'e'), '\\u{1F600}'.replace(/./gu, 'x'), '\\u{1F600}'.replace(/./g, 'x'), lead.replace(/./g, 'x'), lead.replace(/./u, 'x'), pair.replace(/./gu, 'x'), pair.replace(/./g, 'x'), /é/i.test('É')].join('|'); } return out; } f()",
        want: "true|s,li|ee|x|xx|x|x|x|xx|true",
    },
    Case {
        what: "lastIndex that is not a number or not writable stays off the fast path",
        text: "function f() { const r = /a/g; let n = 0; let t = false; let order = ''; for (let i = 0; i < 80; i++) { const arg = { toString() { order = 's'; return 'aba'; } }; r.lastIndex = { valueOf() { order += ',li'; n++; return 1; } }; t = r.test(arg); } const frozen = /a/g; Object.freeze(frozen); let msg = ''; for (let i = 0; i < 40; i++) { try { frozen.test('a'); msg = 'no'; } catch (e) { msg = e.name + ':' + e.message; } } let rep = ''; try { rep = 'a'.replace(frozen, 'b'); } catch (e) { rep = e.name + ':' + e.message; } const plain = /a/; Object.freeze(plain); return [t, n, order, r.lastIndex, msg, rep, plain.test('a')].join('|'); } f()",
        want: "true|80|s,li|3|TypeError:Cannot assign to read only property 'lastIndex'|TypeError:Cannot assign to read only property 'lastIndex'|true",
    },
    Case {
        what: "an own exec and a replaced exec are not the fast path",
        text: "function f() { const r = /a/g; r.exec = function () { return null; }; let last = ''; for (let i = 0; i < 80; i++) last = String(r.test('a')) + ':' + 'a'.replace(r, 'b') + ':' + r.lastIndex; return last; } f()",
        want: "false:a:0",
    },
    Case {
        what: "one call site alternates between two natives",
        text: "function f() { const re = /\\d+/; const box = { test: parseInt }; function step(r, s) { return r.test(s); } let n = 0; for (let i = 0; i < 200; i++) { const v = step(i % 2 ? re : box, '10'); n += i % 2 ? (v ? 1 : 0) : (v === 10 ? 1 : 0); } return n; } f()",
        want: "200",
    },
    Case {
        what: "a literal whose pattern is a lone surrogate stays distinct across compile",
        text: "function f() { const lead = String.fromCharCode(0xD800); const src = 'var r = /' + lead + '/g; [r.source.charCodeAt(0), r.flags, r.test(lead), r.lastIndex, r.source.length].join(\":\")'; let out = ''; for (let i = 0; i < 80; i++) out = eval(src); const a = eval('/' + lead + '/'); const b = eval('/' + lead + '/'); a.compile('b'); const c = eval('/' + lead + '/'); return [out, a !== b, a.source, c.source.charCodeAt(0), c.source.length, c.flags, a !== c].join('|'); } f()",
        want: "55296:g:true:1:1|true|b|55296|1||true",
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
    let r = isolate.run(&mut executor, &cx, &Source { text, file: "jit_regexp.js" }).unwrap_or_else(|e| panic!("script failed ({}): {e:?}\n{text}", tier.label()));
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
    let script = "function f() { let out = ''; for (let i = 0; i < 200; i++) { const r = /ab/g; r.lastIndex = 3; r.x = i; const n = /ab/g; out = [r.lastIndex, r.source, r.flags, n.lastIndex, n.x === undefined, n !== r, /ab/i.flags, /ab/i !== r].join('|'); } return out; } f()";
    let want = "3|ab|g|0|true|true|i|true";
    for tier in [Tier::Interpreter, Tier::Immediate, Tier::Default] {
        let platform = DefaultPlatform::default();
        let mut isolate = Isolate::new(&platform, CreateParams::default());
        let mut executor = match tier {
            Tier::Interpreter => Executor::default(),
            Tier::Immediate => Executor::default().with_tier(j8jit::Jit::from(0)),
            Tier::Default => Executor::default().with_tier(j8jit::Jit::default()),
        };
        let (ca, cb) = isolate.enter(|scope| (scope.persist(scope.context(&Builtins)), scope.persist(scope.context(&Builtins))));
        let ra = isolate.run(&mut executor, &ca, &Source { text: script, file: "realm-a.js" }).unwrap_or_else(|e| panic!("realm a failed ({}): {e:?}", tier.label()));
        let rb = isolate.run(&mut executor, &cb, &Source { text: script, file: "realm-b.js" }).unwrap_or_else(|e| panic!("realm b failed ({}): {e:?}", tier.label()));
        let (va, vb) = isolate.enter(|scope| {
            let va = {
                let mut a = scope.agent(scope.context_of(&ca), &executor);
                String::from(&**a.to_string(a.local(&ra)).expect("realm a ToString"))
            };
            let vb = {
                let mut b = scope.agent(scope.context_of(&cb), &executor);
                String::from(&**b.to_string(b.local(&rb)).expect("realm b ToString"))
            };
            (va, vb)
        });
        assert_eq!(va, want, "realm a ({})", tier.label());
        assert_eq!(vb, want, "realm b ({})", tier.label());
    }
}

#[test]
fn compiled_regexp_literals_match_the_interpreter() {
    let ran = std::thread::Builder::new().stack_size(16 << 20).spawn(run_cases).expect("a thread with room for the compiled frames");
    ran.join().expect("the cases finish");
}
