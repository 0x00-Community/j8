#![cfg(all(target_os = "linux", feature = "crossheap"))]

use std::num::NonZeroUsize;

use j8framework::{CreateParams, Isolate};
use j8platform::DefaultPlatform;
use paragc::{CrossHeapSpace, Shared, Slot, Trace, cross_heap_brand};

const THREADS: NonZeroUsize = NonZeroUsize::new(2).unwrap();

#[repr(C)]
#[derive(Trace, Shared)]
struct Payload<'gc> {
    value: u64,
    next: Slot<'gc, Payload<'gc>>,
}

cross_heap_brand!(PayloadX = for<'p> Payload<'p>);

#[derive(Trace)]
struct Local {
    value: u64,
}

#[test]
fn shared_objects_are_signed_when_the_turn_ends() {
    let space = CrossHeapSpace::reserve_fixed().unwrap();
    let platform = DefaultPlatform::default();
    let mut isolate = Isolate::new(&platform, CreateParams::default());
    isolate.attach_cross_heap(space.create_anonymous(THREADS).unwrap()).unwrap();
    let tickets = isolate.enter(|scope| {
        let m = scope.mc.mutator();
        let tail = m.alloc_shared(Payload { value: 1, next: None.into() });
        let head = m.alloc_shared(Payload { value: 42, next: Some(tail).into() });
        let local = m.alloc(Local { value: 7 });
        (scope.shares.share(scope.mc, head), scope.shares.share(scope.mc, local))
    });
    let signed = isolate.take_signatures();
    assert_eq!(signed.len(), 2);
    let find = |ticket: u64| signed.iter().find(|(t, _)| *t == ticket).and_then(|(_, s)| *s);
    let sig = find(tickets.0).expect("a shareable object is signed once promoted");
    assert!(find(tickets.1).is_none(), "a private object gets no signature");
    let read = |isolate: &Isolate<'_>| isolate.cross_heap().expect("attached").mutate(|cm| cm.resolve::<PayloadX>(&sig).map(|p| (p.value, p.next.get().map(|n| n.value))));
    assert_eq!(read(&isolate), Some((42, Some(1))), "the receiver finds the object and what it points at");
    isolate.collect();
    assert_eq!(read(&isolate), None, "a signature keeps nothing alive");
}
