#![cfg(any(target_arch = "x86_64", target_arch = "aarch64"))]

use std::fmt::Write;

use j8builtins::Builtins;
use j8executor::Executor;
use j8framework::{CreateParams, Isolate, Source};
use j8platform::DefaultPlatform;

#[derive(Clone, Copy)]
enum Tier {
    Interpreter,
    Immediate,
    #[cfg(windows)]
    Default,
}

fn eval(text: &str, tier: Tier) -> String {
    let platform = DefaultPlatform::default();
    let mut isolate = Isolate::new(&platform, CreateParams::default());
    let mut executor = match tier {
        Tier::Interpreter => Executor::default(),
        Tier::Immediate => Executor::default().with_tier(j8jit::Jit::from(0)),
        #[cfg(windows)]
        Tier::Default => Executor::default().with_tier(j8jit::Jit::default()),
    };
    let cx = isolate.enter(|scope| scope.persist(scope.context(&Builtins)));
    let result = isolate.run(&mut executor, &cx, &Source { text, file: "stack.js" }).expect("the script runs");
    isolate.enter(|scope| {
        let realm = scope.context_of(&cx);
        let mut a = scope.agent(realm, &executor);
        let v = a.local(&result);
        String::from(&**a.to_string(v).expect("ToString"))
    })
}

fn wide(locals: u32, then: &str) -> String {
    let mut text = String::from("function wide(n, d) {\n  if (d > 0) return wide(n, d - 1) + 1;\n");
    for i in 0..locals {
        writeln!(text, "  let v{i} = n ^ {i};").expect("a String takes any text");
    }
    text.push_str("  let s = 0;\n");
    for i in 0..locals {
        writeln!(text, "  s = s ^ v{i};").expect("a String takes any text");
    }
    text.push_str("  return s;\n}\n");
    text.push_str(then);
    text
}

#[test]
fn frames_of_many_pages_run_below_the_stack_touched_so_far() {
    let text = wide(10_000, "wide(0x123456, 4)");
    assert_eq!(eval(&text, Tier::Interpreter), "4", "interpreter");
    assert_eq!(eval(&text, Tier::Immediate), "4", "four nested 80 KB frames, each calling out before it writes its slots");
}

#[test]
fn deep_recursion_is_a_range_error() {
    let text = "function rec(n) { if (n <= 0) return 0; return rec(n - 1) + 1; } try { rec(100000); 'returned'; } catch (e) { e instanceof RangeError ? 'range' : 'other'; }";
    assert_eq!(eval(text, Tier::Immediate), "range");
}

#[cfg(windows)]
#[test]
fn recursion_on_a_stack_the_size_of_a_main_thread_is_a_range_error() {
    let text = wide(2_000, "try { wide(0x123456, 1e9); 'returned'; } catch (e) { e instanceof RangeError ? 'range' : 'other'; }");
    let ran = std::thread::Builder::new().stack_size(1 << 20).spawn(move || eval(&text, Tier::Default)).expect("a thread with 1 MiB of stack").join();
    assert_eq!(ran.expect("the thread finishes"), "range");
}
