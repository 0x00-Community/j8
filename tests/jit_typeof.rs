#![cfg(any(target_arch = "x86_64", target_arch = "aarch64"))]

use j8builtins::Builtins;
use j8executor::Executor;
use j8framework::{CreateParams, Error, Isolate, Source};
use j8platform::DefaultPlatform;

struct Case {
    what: &'static str,
    text: &'static str,
    want: Option<&'static str>,
}

const CASES: &[Case] = &[
    Case { what: "an undeclared name inside a function", text: "function f() { return typeof notDeclared; } let r; for (let i = 0; i < 3000; i++) r = f(); r", want: Some("undefined") },
    Case { what: "an undeclared name in a top-level loop", text: "let r; for (let i = 0; i < 5000; i++) r = typeof notDeclared; r", want: Some("undefined") },
    Case { what: "a global that exists", text: "function f() { return typeof Math; } let r; for (let i = 0; i < 3000; i++) r = f(); r", want: Some("object") },
    Case { what: "a name missing on the first call and defined before the loop gets hot", text: "function f() { return typeof later; } const a = f(); globalThis.later = 1; let b; for (let i = 0; i < 3000; i++) b = f(); [a, b].join()", want: Some("undefined,number") },
    Case { what: "a plain read of an undeclared name still throws", text: "function g() { return notDeclared; } for (let i = 0; i < 3000; i++) g();", want: Some("Uncaught ReferenceError: notDeclared is not defined") },
    Case { what: "typeof does not make a later plain read of the same name succeed", text: "function h() { const t = typeof notDeclared; return notDeclared; } for (let i = 0; i < 3000; i++) h();", want: Some("Uncaught ReferenceError: notDeclared is not defined") },
    Case { what: "a plain read, then typeof of the same global", text: "function p() { const m = Math; return typeof Math; } let r; for (let i = 0; i < 3000; i++) r = p(); r", want: Some("object") },
    Case { what: "typeof of a name in its temporal dead zone throws", text: "function k() { return typeof lateLet; } for (let i = 0; i < 3000; i++) k(); let lateLet = 1;", want: None },
];

fn outcome(jit: bool, text: &str) -> String {
    let platform = DefaultPlatform::default();
    let mut isolate = Isolate::new(&platform, CreateParams::default());
    let executor = if jit { Executor::default().with_tier(j8jit::Jit::from(0)) } else { Executor::default() };
    let cx = isolate.enter(|scope| scope.mc.stash::<j8framework::ContextBrand>(scope.context(&Builtins)));
    let r = executor.eval(&mut isolate, &cx, &Source { text, file: "typeof.js" });
    isolate.enter(|scope| {
        let context = scope.mc.fetch(&cx);
        let mut a = scope.agent(context, &executor);
        match r {
            Ok(g) => {
                let v = a.local(&g);
                String::from(&**a.to_string(v).expect("ToString"))
            }
            Err(Error::Exception) => {
                let v = a.take_exception();
                let s = String::from(&**a.to_string(v).expect("ToString"));
                format!("Uncaught {s}")
            }
            Err(e) => panic!("not a completion: {e}"),
        }
    })
}

#[test]
fn typeof_global_agrees() {
    let mut bad = Vec::new();
    for case in CASES {
        let interp = outcome(false, case.text);
        let compiled = outcome(true, case.text);
        if interp != compiled {
            bad.push(format!("{}: the tiers differ\n  interpreter: {interp}\n  jit: {compiled}\n  script: {}", case.what, case.text));
        }
        if let Some(want) = case.want
            && interp != want
        {
            bad.push(format!("{}: the interpreter gives `{interp}`, want `{want}`\n  script: {}", case.what, case.text));
        }
    }
    assert!(bad.is_empty(), "{}", bad.join("\n"));
}
