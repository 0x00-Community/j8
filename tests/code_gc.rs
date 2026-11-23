use j8builtins::Builtins;
use j8executor::Executor;
use j8framework::{CreateParams, Isolate, Source};
use j8platform::DefaultPlatform;

const FUNCTIONS: usize = 200;

const MAKE: &str = "globalThis.keep = []; for (let i = 0; i < 200; i++) keep.push(eval('(function (x) { return { a: x, b: ' + i + ' }; })'));";

const CALL: &str = "(() => { let s = 0; for (let i = 0; i < 200; i++) { const o = keep[i](i); s += o.a + o.b; } return s; })()";

const SUM: f64 = 39_800.0;

fn number(isolate: &mut Isolate<'_>, executor: &mut Executor, cx: &j8framework::ContextRoot, text: &str) -> f64 {
    let g = isolate.run(executor, cx, &Source { text, file: "code_gc.js" }).expect("runs");
    isolate.enter(|scope| {
        let realm = scope.context_of(cx);
        let a = scope.agent(realm, &*executor);
        a.local(&g).as_number().expect("a number")
    })
}

fn moves_and_frees_code(mut executor: Executor) {
    let platform = DefaultPlatform::default();
    let mut isolate = Isolate::new(&platform, CreateParams::default());
    let cx = isolate.enter(|scope| scope.persist(scope.context(&Builtins)));
    isolate.run(&mut executor, &cx, &Source { text: MAKE, file: "code_gc.js" }).expect("runs");
    let made = executor.live_functions();
    assert!(made >= FUNCTIONS, "{made} live functions after making {FUNCTIONS}");
    for _ in 0..3 {
        assert_eq!(number(&mut isolate, &mut executor, &cx, CALL), SUM);
        isolate.collect();
    }
    assert_eq!(number(&mut isolate, &mut executor, &cx, CALL), SUM);
    isolate.run(&mut executor, &cx, &Source { text: "keep = null;", file: "code_gc.js" }).expect("runs");
    isolate.collect();
    let left = executor.live_functions();
    assert!(left + FUNCTIONS <= made, "{left} of {made} functions still live after dropping {FUNCTIONS}");
}

#[test]
fn interpreted_code_moves_and_dies() {
    moves_and_frees_code(Executor::default());
}

#[test]
fn compiled_code_moves_and_dies() {
    moves_and_frees_code(Executor::default().with_tier(j8jit::Jit::from(0)));
}
