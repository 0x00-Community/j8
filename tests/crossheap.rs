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

#[test]
fn shared_objects_are_signed_when_the_turn_ends() {
    let space = CrossHeapSpace::reserve_fixed().unwrap();
    let platform = DefaultPlatform::default();
    let mut isolate = Isolate::new(&platform, CreateParams::default());
    isolate.attach_cross_heap(unsafe { space.create_anonymous_unchecked(THREADS) }.unwrap()).unwrap();
    let tickets = isolate.enter(|scope| {
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
