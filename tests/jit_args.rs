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
        what: "fourteen arguments to a built-in, the most the helper buffer holds",
        text: "function f() { return String.fromCharCode(97, 98, 99, 100, 101, 102, 103, 104, 105, 106, 107, 108, 109, 110).length; } let n = 0; for (let i = 0; i < 5000; i++) n += f(); n",
        want: "70000",
    },
    Case {
        what: "fifteen arguments to a built-in",
        text: "function f() { return String.fromCharCode(97, 98, 99, 100, 101, 102, 103, 104, 105, 106, 107, 108, 109, 110, 111).length; } let n = 0; for (let i = 0; i < 5000; i++) n += f(); n",
        want: "75000",
    },
    Case {
        what: "thirty arguments to a built-in in a top-level loop",
        text: "let n = 0; for (let i = 0; i < 5000; i++) n += Math.max(i, 1, 2, 3, 4, 5, 6, 7, 8, 9, 10, 11, 12, 13, 14, 15, 16, 17, 18, 19, 20, 21, 22, 23, 24, 25, 26, 27, 28, 29); n",
        want: "12497935",
    },
    Case {
        what: "twenty arguments to a closure that reads the last one",
        text: "function g(a, b, c, d, e, f, g, h, i, j, k, l, m, n, o, p, q, r, s, t) { return a + t; } let n = 0; for (let i = 0; i < 5000; i++) n += g(i, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 1); n",
        want: "12502500",
    },
    Case {
        what: "sixteen arguments to a constructor",
        text: "function K() { this.n = arguments.length; } function mk() { return new K(1, 2, 3, 4, 5, 6, 7, 8, 9, 10, 11, 12, 13, 14, 15, 16).n; } let n = 0; for (let i = 0; i < 5000; i++) n += mk(); n",
        want: "80000",
    },
    Case {
        what: "twenty arguments to a built-in constructor",
        text: "function mk() { return new Array(1, 2, 3, 4, 5, 6, 7, 8, 9, 10, 11, 12, 13, 14, 15, 16, 17, 18, 19, 20).length; } let n = 0; for (let i = 0; i < 5000; i++) n += mk(); n",
        want: "100000",
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
    let r = isolate.run(&mut executor, &cx, &Source { text, file: "jit_args.js" }).unwrap_or_else(|e| panic!("script failed ({}): {e:?}\n{text}", tier.label()));
    isolate.enter(|scope| {
        let realm = scope.context_of(&cx);
        let mut a = scope.agent(realm, &executor);
        let v = a.local(&r);
        String::from(&**a.to_string(v).unwrap_or_else(|e| panic!("ToString failed ({}): {e:?}\n{text}", tier.label())))
    })
}

#[test]
fn long_argument_lists_match_the_interpreter() {
    for case in CASES {
        for tier in [Tier::Interpreter, Tier::Immediate, Tier::Default] {
            assert_eq!(completion(case.text, tier), case.want, "{} ({}): {}", case.what, tier.label(), case.text);
        }
    }
}
