use std::time::Duration;

use j8builtins::Builtins;
use j8executor::Executor;
use j8framework::{CreateParams, Error, Isolate, Source};
use j8platform::DefaultPlatform;

const SPIN: Duration = Duration::from_millis(50);

#[test]
fn terminate_from_another_thread() {
    let platform = DefaultPlatform::default();
    let mut isolate = Isolate::new(&platform, CreateParams::default());
    let mut executor = Executor::default().with_tier(j8jit::Jit::from(0));
    let cx = isolate.enter(|scope| scope.persist(scope.context(&Builtins)));
    let handle = isolate.handle();
    let text = "function spin() { let n = 0; for (;;) n++; } try { spin(); } catch (e) { globalThis.caught = 1; } finally { globalThis.fin = 1; }";
    let r = std::thread::scope(|s| {
        s.spawn(|| {
            std::thread::sleep(SPIN);
            handle.terminate();
        });
        isolate.run(&mut executor, &cx, &Source { text, file: "spin.js" })
    });
    assert!(matches!(r, Err(Error::Terminated)), "{r:?}");
    assert!(!handle.is_terminating(), "reset when leaving the outermost entry");
    let after = isolate.run(&mut executor, &cx, &Source { text: "`${typeof caught} ${typeof fin} ${1 + 1}`", file: "after.js" }).expect("the isolate stays usable");
    let text = isolate.enter(|scope| {
        let realm = scope.context_of(&cx);
        let mut a = scope.agent(realm, &executor);
        let v = a.local(&after);
        String::from(&**a.to_string(v).expect("ToString"))
    });
    assert_eq!(text, "undefined undefined 2");
}
