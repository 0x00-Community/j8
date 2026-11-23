use j8builtins::Builtins;
use j8executor::Executor;
use j8framework::{CreateParams, Isolate, ContextBrand, ScopeGlobal, Source};
use j8platform::DefaultPlatform;

const FUNCTIONS: usize = 200;

const MAKE: &str = "globalThis.keep = []; for (let i = 0; i < 200; i++) keep.push(eval('(function (x) { return { a: x, b: ' + i + ' }; })'));";

const CALL: &str = "(() => { let s = 0; for (let i = 0; i < 200; i++) { const o = keep[i](i); s += o.a + o.b; } return s; })()";

const SUM: f64 = 39_800.0;

fn number<'p>(isolate: &mut Isolate<'p>, executor: &Executor<'p>, cx: &ScopeGlobal<'p, ContextBrand>, text: &str) -> f64 {
    let g = executor.eval(isolate, cx, &Source { text, file: "code_gc.js" }).expect("runs");
    isolate.enter(|scope| {
        let context = scope.mc.fetch(cx);
        let a = scope.agent(context, executor);
        a.local(&g).as_number().expect("a number")
    })
}

fn moves_and_frees_code(tiered: bool) {
    let platform = DefaultPlatform::default();
    let mut isolate = Isolate::new(&platform, CreateParams::default());
    let executor = if tiered { Executor::default().with_tier(j8jit::Jit::from(0)) } else { Executor::default() };
    let cx = isolate.enter(|scope| scope.mc.stash::<j8framework::ContextBrand>(scope.context(&Builtins)));
    executor.eval(&mut isolate, &cx, &Source { text: MAKE, file: "code_gc.js" }).expect("runs");
    let made = executor.live_functions();
    assert!(made >= FUNCTIONS, "{made} live functions after making {FUNCTIONS}");
    for _ in 0..3 {
        assert_eq!(number(&mut isolate, &executor, &cx, CALL), SUM);
        isolate.collect();
    }
    assert_eq!(number(&mut isolate, &executor, &cx, CALL), SUM);
    executor.eval(&mut isolate, &cx, &Source { text: "keep = null;", file: "code_gc.js" }).expect("runs");
    isolate.collect();
    let left = executor.live_functions();
    assert!(left + FUNCTIONS <= made, "{left} of {made} functions still live after dropping {FUNCTIONS}");
}

#[test]
fn interpreted_code_moves_and_dies() {
    moves_and_frees_code(false);
}

#[test]
fn compiled_code_moves_and_dies() {
    moves_and_frees_code(true);
}
