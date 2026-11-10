use alloc::vec::Vec;

use gc_arena::{Gc, Lock};
use r8frame::{Agent, Error, ErrorKind, JsObject, ObjectKind, Reaction, Value};

use crate::{Args, Installer};

/// `Promise` 与原型方法。
struct Promises;

impl Promises {
    fn this<'gc>(a: &Agent<'gc, '_>, this: Value<'gc>) -> Result<JsObject<'gc>, Error> {
        match this {
            Value::Object(o) if matches!(o.borrow().kind, ObjectKind::Promise { .. }) => Ok(o),
            _ => Err(a.throw(ErrorKind::Type, "Promise.prototype method called on a non-promise")),
        }
    }

    /// `new Promise(executor)`：同步调 `executor(resolve, reject)`，它抛错就拒绝。
    fn construct<'gc>(a: &mut Agent<'gc, '_>, _: Value<'gc>, args: &[Value<'gc>]) -> Result<Value<'gc>, Error> {
        let executor = args.at(0);
        if !a.is_callable(executor) {
            return Err(a.throw(ErrorKind::Type, "Promise resolver is not a function"));
        }
        let p = a.new_promise();
        let (resolve, reject) = a.promise_resolvers(p);
        match a.call(executor, Value::Undefined, &[resolve.into(), reject.into()]) {
            Ok(_) => {}
            Err(Error::Exception(g)) => {
                let reason = a.local(&g);
                a.run_resolver(reject, reason)?;
            }
            Err(e) => return Err(e),
        }
        Ok(p.into())
    }

    fn resolve<'gc>(a: &mut Agent<'gc, '_>, _: Value<'gc>, args: &[Value<'gc>]) -> Result<Value<'gc>, Error> {
        Ok(a.promise_resolve(args.at(0))?.into())
    }

    fn reject<'gc>(a: &mut Agent<'gc, '_>, _: Value<'gc>, args: &[Value<'gc>]) -> Result<Value<'gc>, Error> {
        let p = a.new_promise();
        a.reject_promise(p, args.at(0));
        Ok(p.into())
    }

    /// `Promise.all(iterable)`。
    fn all<'gc>(a: &mut Agent<'gc, '_>, _: Value<'gc>, args: &[Value<'gc>]) -> Result<Value<'gc>, Error> {
        let items = a.to_list(args.at(0))?;
        let result = a.new_promise();
        let values = a.array(alloc::vec![Value::Undefined; items.len()]);
        if items.is_empty() {
            a.resolve_promise(result, values.into())?;
            return Ok(result.into());
        }
        let remaining = Gc::new(a.mc, Lock::new(u32::try_from(items.len()).unwrap_or(u32::MAX)));
        for (index, v) in (0u32..).zip(items) {
            let p = a.promise_resolve(v)?;
            a.perform_then(p, Reaction::All { values, index, remaining, result });
        }
        Ok(result.into())
    }

    /// `Promise.race(iterable)`：谁先落定用谁。
    fn race<'gc>(a: &mut Agent<'gc, '_>, _: Value<'gc>, args: &[Value<'gc>]) -> Result<Value<'gc>, Error> {
        let items: Vec<Value<'gc>> = a.to_list(args.at(0))?;
        let result = a.new_promise();
        for v in items {
            let p = a.promise_resolve(v)?;
            a.perform_then(p, Reaction::Then { on_fulfilled: Value::Undefined, on_rejected: Value::Undefined, derived: Some(result) });
        }
        Ok(result.into())
    }

    fn then<'gc>(a: &mut Agent<'gc, '_>, this: Value<'gc>, args: &[Value<'gc>]) -> Result<Value<'gc>, Error> {
        let p = Self::this(a, this)?;
        let derived = a.new_promise();
        a.perform_then(p, Reaction::Then { on_fulfilled: args.at(0), on_rejected: args.at(1), derived: Some(derived) });
        Ok(derived.into())
    }

    fn catch<'gc>(a: &mut Agent<'gc, '_>, this: Value<'gc>, args: &[Value<'gc>]) -> Result<Value<'gc>, Error> {
        Self::then(a, this, &[Value::Undefined, args.at(0)])
    }

    fn finally<'gc>(a: &mut Agent<'gc, '_>, this: Value<'gc>, args: &[Value<'gc>]) -> Result<Value<'gc>, Error> {
        let p = Self::this(a, this)?;
        let derived = a.new_promise();
        a.perform_then(p, Reaction::Finally { on_finally: args.at(0), derived });
        Ok(derived.into())
    }
}

impl Installer<'_, '_> {
    pub(crate) fn promise(&mut self) {
        let proto = self.realm.promise_proto;
        let ctor = self.constructor("Promise", Promises::construct, proto);
        self.method(ctor, "resolve", Promises::resolve);
        self.method(ctor, "reject", Promises::reject);
        self.method(ctor, "all", Promises::all);
        self.method(ctor, "race", Promises::race);
        self.method(proto, "then", Promises::then);
        self.method(proto, "catch", Promises::catch);
        self.method(proto, "finally", Promises::finally);
    }
}
