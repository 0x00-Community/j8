use alloc::vec::Vec;

use j8framework::ValueKind;
use j8framework::{Agent, Capability, Error, ErrorKind, IterRecord, JsObject, NativeClosure, ObjectData, ObjectKind, Reaction, Resume, Value};

use crate::{Args, Installer};

struct AsyncGenerators;

impl AsyncGenerators {
    fn enqueue<'gc>(a: &mut Agent<'gc, '_>, this: Value<'gc>, mode: Resume, v: Value<'gc>) -> Result<Value<'gc>, Error> {
        let co = match this.kind() {
            ValueKind::Object(o) if matches!(&o.borrow().kind, ObjectKind::Coroutine(c) if c.requests.is_some()) => o,
            _ => {
                let p = a.new_promise();
                let e = a.error(ErrorKind::Type, "AsyncGenerator method called on an incompatible receiver");
                a.reject_promise(p, e.into());
                return Ok(p.into());
            }
        };
        Ok(a.async_generator_enqueue(co, mode, v)?.into())
    }

    fn next<'gc>(a: &mut Agent<'gc, '_>, this: Value<'gc>, args: &[Value<'gc>]) -> Result<Value<'gc>, Error> {
        Self::enqueue(a, this, Resume::Next, args.at(0))
    }

    fn return_<'gc>(a: &mut Agent<'gc, '_>, this: Value<'gc>, args: &[Value<'gc>]) -> Result<Value<'gc>, Error> {
        Self::enqueue(a, this, Resume::Return, args.at(0))
    }

    fn throw<'gc>(a: &mut Agent<'gc, '_>, this: Value<'gc>, args: &[Value<'gc>]) -> Result<Value<'gc>, Error> {
        Self::enqueue(a, this, Resume::Throw, args.at(0))
    }

    fn this_value<'gc>(_: &mut Agent<'gc, '_>, this: Value<'gc>, _: &[Value<'gc>]) -> Result<Value<'gc>, Error> {
        Ok(this)
    }
}

struct AsyncFromSync;

const DONE: usize = 0;
const ITERATOR: usize = 1;

impl AsyncFromSync {
    fn record(this: Value<'_>) -> Option<IterRecord<'_>> {
        match this.as_object()?.borrow().kind {
            ObjectKind::AsyncFromSyncIterator(r) => Some(r),
            _ => None,
        }
    }

    fn capability<'gc>(a: &mut Agent<'gc, '_>) -> (JsObject<'gc>, Capability<'gc>) {
        let p = a.new_promise();
        let (resolve, reject) = a.promise_resolvers(p);
        (p, Capability { promise: p.into(), resolve: resolve.into(), reject: reject.into() })
    }

    fn rejected<'gc>(a: &mut Agent<'gc, '_>, p: JsObject<'gc>, e: Error) -> Result<Value<'gc>, Error> {
        match e {
            Error::Exception(g) => {
                let reason = a.local(&g);
                a.reject_promise(p, reason);
                Ok(p.into())
            }
            e => Err(e),
        }
    }

    fn closure<'gc>(a: &Agent<'gc, '_>, f: j8framework::ClosureFn, slots: Vec<Value<'gc>>) -> Value<'gc> {
        let o = ObjectData { kind: ObjectKind::NativeClosure(NativeClosure { f, slots, constructor: false }), proto: Some(a.realm.function_proto), ..ObjectData::default() }.alloc(a.mc);
        a.realm.name_function(a.mc, o, "", Some(1));
        o.into()
    }

    fn slot(f: JsObject<'_>, i: usize) -> Value<'_> {
        match &f.borrow().kind {
            ObjectKind::NativeClosure(c) => c.slots.get(i).copied().unwrap_or_default(),
            _ => Value::Undefined,
        }
    }

    fn unwrap<'gc>(a: &mut Agent<'gc, '_>, f: JsObject<'gc>, _: Value<'gc>, args: &[Value<'gc>]) -> Result<Value<'gc>, Error> {
        let done = a.to_boolean(Self::slot(f, DONE));
        Ok(a.iter_result(args.at(0), done).into())
    }

    fn close<'gc>(a: &mut Agent<'gc, '_>, f: JsObject<'gc>, _: Value<'gc>, args: &[Value<'gc>]) -> Result<Value<'gc>, Error> {
        let reason = args.at(0);
        let _ = a.iterator_close(Self::slot(f, ITERATOR));
        Err(a.throw_value(reason))
    }

    fn continuation<'gc>(a: &mut Agent<'gc, '_>, result: Value<'gc>, p: JsObject<'gc>, cap: Capability<'gc>, rec: IterRecord<'gc>, close_on_rejection: bool) -> Result<Value<'gc>, Error> {
        let done = match a.get_named(result, "done") {
            Ok(d) => a.to_boolean(d),
            Err(e) => return Self::rejected(a, p, e),
        };
        let value = match a.get_named(result, "value") {
            Ok(v) => v,
            Err(e) => return Self::rejected(a, p, e),
        };
        let wrapper = match a.promise_resolve(value) {
            Ok(w) => w,
            Err(e) => {
                if !done && close_on_rejection {
                    let _ = a.iterator_close(rec.iterator);
                }
                return Self::rejected(a, p, e);
            }
        };
        let on_fulfilled = Self::closure(a, Self::unwrap, alloc::vec![done.into()]);
        let on_rejected = if done || !close_on_rejection { Value::Undefined } else { Self::closure(a, Self::close, alloc::vec![Value::Undefined, rec.iterator]) };
        a.perform_then(wrapper, Reaction::Then { on_fulfilled, on_rejected, capability: Some(cap) });
        Ok(p.into())
    }

    fn next<'gc>(a: &mut Agent<'gc, '_>, this: Value<'gc>, args: &[Value<'gc>]) -> Result<Value<'gc>, Error> {
        let (p, cap) = Self::capability(a);
        let Some(rec) = Self::record(this) else { unreachable!("only reachable through CreateAsyncFromSyncIterator") };
        let sent: &[Value<'gc>] = if args.is_empty() { &[] } else { &args[..1] };
        let result = match a.call(rec.next, rec.iterator, sent) {
            Ok(r) if r.as_object().is_some() => r,
            Ok(r) => {
                let what = a.describe(r);
                let e = a.throw(ErrorKind::Type, &alloc::format!("Iterator result {what} is not an object"));
                return Self::rejected(a, p, e);
            }
            Err(e) => return Self::rejected(a, p, e),
        };
        Self::continuation(a, result, p, cap, rec, true)
    }

    fn forward<'gc>(a: &mut Agent<'gc, '_>, this: Value<'gc>, args: &[Value<'gc>], name: &str) -> Result<Value<'gc>, Error> {
        let (p, cap) = Self::capability(a);
        let Some(rec) = Self::record(this) else { unreachable!("only reachable through CreateAsyncFromSyncIterator") };
        let method = match a.get_named(rec.iterator, name) {
            Ok(m) => m,
            Err(e) => return Self::rejected(a, p, e),
        };
        let throw = name == "throw";
        if method.is_nullish() {
            if !throw {
                let r = a.iter_result(args.at(0), true);
                a.resolve_promise(p, r.into())?;
                return Ok(p.into());
            }
            if let Err(e) = a.iterator_close(rec.iterator) {
                return Self::rejected(a, p, e);
            }
            let e = a.throw(ErrorKind::Type, "The iterator does not provide a 'throw' method");
            return Self::rejected(a, p, e);
        }
        let sent: &[Value<'gc>] = if args.is_empty() { &[] } else { &args[..1] };
        let result = match a.call(method, rec.iterator, sent) {
            Ok(r) if r.as_object().is_some() => r,
            Ok(r) => {
                let what = a.describe(r);
                let e = a.throw(ErrorKind::Type, &alloc::format!("Iterator result {what} is not an object"));
                return Self::rejected(a, p, e);
            }
            Err(e) => return Self::rejected(a, p, e),
        };
        Self::continuation(a, result, p, cap, rec, throw)
    }

    fn return_<'gc>(a: &mut Agent<'gc, '_>, this: Value<'gc>, args: &[Value<'gc>]) -> Result<Value<'gc>, Error> {
        Self::forward(a, this, args, "return")
    }

    fn throw<'gc>(a: &mut Agent<'gc, '_>, this: Value<'gc>, args: &[Value<'gc>]) -> Result<Value<'gc>, Error> {
        Self::forward(a, this, args, "throw")
    }
}

impl Installer<'_, '_> {
    pub(crate) fn async_generators(&mut self) {
        let r = self.realm;
        r.symbol_method(self.mc, r.async_iterator_proto, r.symbols.async_iterator, AsyncGenerators::this_value, 0);
        let p = r.async_generator_proto;
        self.method(p, "next", AsyncGenerators::next, 1);
        self.method(p, "return", AsyncGenerators::return_, 1);
        self.method(p, "throw", AsyncGenerators::throw, 1);
        let p = r.async_from_sync_iterator_proto;
        self.method(p, "next", AsyncFromSync::next, 1);
        self.method(p, "return", AsyncFromSync::return_, 1);
        self.method(p, "throw", AsyncFromSync::throw, 1);
    }
}
