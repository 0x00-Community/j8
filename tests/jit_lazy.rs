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
        what: "a fresh closure on each call, its name still lazy when the read is cached",
        text: "function mk() { return function inner() {}; } function nm(f) { return f.name; } let s = 0; for (let i = 0; i < 5000; i++) s += nm(mk()).length; s",
        want: "25000",
    },
    Case { what: "the same read in a top-level loop", text: "let s = 0; for (let i = 0; i < 5000; i++) { const f = function inner() {}; s += f.name.length; } s", want: "25000" },
    Case {
        what: "one site sees a resolved name and a lazy one",
        text: "function nm(f) { return f.name; } const a = function first() {}; nm(a); let s = 0; for (let i = 0; i < 5000; i++) s += nm(i & 1 ? a : function second() {}).length; s",
        want: "27500",
    },
    Case { what: "a constructor's name through `constructor`", text: "function cn(x) { return x.constructor.name; } let s = 0; for (let i = 0; i < 5000; i++) { class K {} s += cn(new K()).length; } s", want: "5000" },
    Case { what: "the name lives on the prototype", text: "function nm(o) { return o.name; } let s = 0; for (let i = 0; i < 5000; i++) s += nm(Object.create(function inner() {})).length; s", want: "25000" },
    Case { what: "an ordinary object's own `name` is a plain value", text: "function nm(o) { return o.name; } let s = 0; for (let i = 0; i < 5000; i++) s += nm({ name: i }); s", want: "12497500" },
    Case { what: "an arrow function's inferred name", text: "let s = 0; for (let i = 0; i < 5000; i++) { const g = () => i; s += g.name.length; } s", want: "5000" },
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
    let r = isolate.run(&mut executor, &cx, &Source { text, file: "jit_lazy.js" }).unwrap_or_else(|e| panic!("script failed ({}): {e:?}\n{text}", tier.label()));
    isolate.enter(|scope| {
        let realm = scope.context_of(&cx);
        let mut a = scope.agent(realm, &executor);
        let v = a.local(&r);
        String::from(&**a.to_string(v).unwrap_or_else(|e| panic!("ToString failed ({}): {e:?}\n{text}", tier.label())))
    })
}

#[test]
fn lazy_name_matches_the_interpreter() {
    for case in CASES {
        for tier in [Tier::Interpreter, Tier::Immediate, Tier::Default] {
            assert_eq!(completion(case.text, tier), case.want, "{} ({}): {}", case.what, tier.label(), case.text);
        }
    }
}
