#![cfg(all(target_os = "linux", feature = "crossheap"))]

use std::num::NonZeroUsize;

use j8framework::{CreateParams, GcSlot, Isolate, JsStr};
use j8platform::DefaultPlatform;
use paragc::{CrossHeapSpace, Shared, Trace, cross_heap_brand};

const THREADS: NonZeroUsize = NonZeroUsize::new(2).unwrap();

#[repr(C)]
#[derive(Trace, Shared)]
struct Payload<'scope> {
    value: u64,
    next: GcSlot<'scope, Payload<'scope>>,
}

cross_heap_brand!(PayloadX = for<'xheap, 'p> Payload<'p>);
cross_heap_brand!(StrX = for<'xheap, 'p> JsStr<'p>);

#[derive(Trace)]
struct Local {
    value: u64,
}

fn shared_objects_are_signed_when_the_turn_ends() {
    let space = CrossHeapSpace::reserve_fixed().unwrap();
    let platform = DefaultPlatform::default();
    let mut isolate = Isolate::new(&platform, CreateParams::default());
    isolate.attach_cross_heap(unsafe { space.create_anonymous_unchecked(THREADS) }.unwrap()).unwrap();
    let executor = j8executor::Executor::default();
    let cx = isolate.create_context(&j8builtins::Builtins);
    let tickets = isolate.enter(&cx, &executor, |scope| {
        let m = scope.mc.gc_scope();
        let tail = m.alloc_shared(Payload { value: 1, next: None.into() });
        let head = m.alloc_shared(Payload { value: 42, next: Some(tail).into() });
        let local = m.alloc(Local { value: 7 });
        let long = (0..100).fold(JsStr::alloc(scope.mc, ""), |s, i| JsStr::concat(scope.mc, s, JsStr::alloc(scope.mc, &format!("ab{i}"))));
        let wide = JsStr::alloc(scope.mc, "中文ñ😀");
        (scope.shares.share(scope.mc, head), scope.shares.share(scope.mc, local), scope.shares.share(scope.mc, long), scope.shares.share(scope.mc, wide))
    });
    let signed = isolate.take_signatures();
    assert_eq!(signed.len(), 4);
    let find = |ticket: u64| signed.iter().find(|(t, _)| *t == ticket).and_then(|(_, s)| *s);
    let sig = find(tickets.0).expect("a shareable object is signed once promoted");
    assert!(find(tickets.1).is_none(), "a private object gets no signature");
    let read = |isolate: &Isolate<'_>| isolate.cross_heap().expect("attached").scope(|cm| cm.resolve::<PayloadX>(&sig).map(|p| (p.value, p.next.get().map(|n| n.value))));
    assert_eq!(read(&isolate), Some((42, Some(1))), "the receiver finds the object and what it points at");
    let text = |ticket: u64| {
        let sig = find(ticket).expect("a string is shareable");
        isolate.cross_heap().expect("attached").scope(|cm| cm.resolve::<StrX>(&sig).map(|s| (s.as_str().to_owned(), s.units().iter().collect::<Vec<u16>>())))
    };
    let long: String = (0..100).map(|i| format!("ab{i}")).collect();
    assert_eq!(text(tickets.2), Some((long.clone(), long.encode_utf16().collect())), "a string built by extending a buffer travels with its buffer");
    assert_eq!(text(tickets.3), Some(("中文ñ😀".to_owned(), "中文ñ😀".encode_utf16().collect())), "a wide string keeps its UTF-16 units");
    isolate.collect();
    assert_eq!(read(&isolate), None, "a signature keeps nothing alive");
}

fn two_turns(setup: &str, check: &str) -> String {
    let space = CrossHeapSpace::reserve_fixed().unwrap();
    let platform = DefaultPlatform::default();
    let mut isolate = Isolate::new(&platform, CreateParams::default());
    isolate.attach_cross_heap(unsafe { space.create_anonymous_unchecked(THREADS) }.unwrap()).unwrap();
    let executor = j8executor::Executor::default();
    let cx = isolate.create_context(&j8builtins::Builtins);
    executor.eval(&mut isolate, &cx, &j8framework::Source { text: setup, file: "setup.js" }).unwrap();
    isolate.collect();
    let r = executor.eval(&mut isolate, &cx, &j8framework::Source { text: check, file: "check.js" });
    let out = isolate.enter(&cx, &executor, |scope| match r {
        Ok(g) => scope.to_string(*scope.mc.fetch(&g)).map_or_else(|_| String::from("?"), |s| String::from(&**s)),
        Err(e) => {
            let v = scope.take_exception();
            format!("error {e:?}: {}", scope.to_string(v).map_or_else(|_| String::from("?"), |s| String::from(&**s)))
        }
    });
    isolate.collect();
    out
}

fn js_objects_keep_working_after_promotion() {
    assert_eq!(two_turns("var o = {a: 1, b: 'x', c: [1, 2, {d: true}]};", "o.a + o.b + o.c[2].d + o.c.length"), "1xtrue3");
    assert_eq!(two_turns("var o = {f: function () { return 5; }};", "o.f()"), "5");
    assert_eq!(two_turns("function F() {} F.prototype.g = function () { return 6; }; var o = new F();", "o.g()"), "6");
    assert_eq!(two_turns("var o = {};", "o.toString()"), "[object Object]");
    assert_eq!(two_turns("var a = [3, 1, 2];", "a.sort().join()"), "1,2,3");
    assert_eq!(two_turns("var z = 1;", "Object.getOwnPropertyNames(JSON).join() + JSON.stringify({a: [1, 'x']})"), "parse,stringify{\"a\":[1,\"x\"]}");
    assert_eq!(two_turns("var o = {a: 1}; o.b = 2; delete o.a;", "o.c = 3; Object.keys(o).join()"), "b,c");
}

#[test]
fn cross_heap_scenarios() {
    shared_objects_are_signed_when_the_turn_ends();
    js_objects_keep_working_after_promotion();
}
