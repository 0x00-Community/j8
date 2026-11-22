#![cfg(target_arch = "x86_64")]

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

#[test]
fn builtins_calling_compiled_code_match_the_interpreter() {
    for case in CASES {
        for tier in [Tier::Interpreter, Tier::Immediate, Tier::Default] {
            assert_eq!(completion(case.text, tier), case.want, "{} ({}): {}", case.what, tier.label(), case.text);
        }
    }
}
