use r8framework::ValueKind;
use alloc::vec::Vec;

use r8framework::{Agent, Capability, Error, ErrorKind, JsObject, Key, NativeClosure, ObjectData, ObjectKind, Property, Reaction, Slot, Value};

use crate::buffer::Buffers;
use crate::{Args, Installer};

/// `Promise` and its prototype methods (via NewPromiseCapability per spec, so subclasses work too).
struct Promises;

/// Kind of combinator.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Combinator {
    All,
    AllSettled,
    Any,
    Race,
}

/// State slots of element functions (indices into the NativeClosure slots).
mod slot {
    /// Shared "already called" flag (shared by an element's resolve / reject functions): array `[bool]`
    pub const CALLED: usize = 0;
    pub const INDEX: usize = 1;
    /// Result list (internal array)
    pub const VALUES: usize = 2;
    /// The capability record's resolve (reject for Any)
    pub const SETTLE: usize = 3;
    /// Remaining count: array `[n]`
    pub const REMAINING: usize = 4;
}

impl Promises {
    fn is_intrinsic<'gc>(a: &Agent<'gc, '_>, c: Value<'gc>) -> bool {
        matches!(c.kind(), ValueKind::Object(o) if r8gc::Gc::ptr_eq(o, a.realm.promise_ctor))
    }

    fn closure<'gc>(a: &Agent<'gc, '_>, f: r8framework::ClosureFn, slots: Vec<Value<'gc>>, length: u32) -> JsObject<'gc> {
        let o = ObjectData { kind: ObjectKind::NativeClosure(NativeClosure { f, slots }), proto: Some(a.realm.function_proto), ..ObjectData::default() }.alloc(a.mc);
        a.realm.name_function(a.mc, o, "", Some(length));
        o
    }

    fn slots(f: JsObject<'_>) -> Vec<Value<'_>> {
        match &f.borrow().kind {
            ObjectKind::NativeClosure(c) => c.slots.clone(),
            _ => Vec::new(),
        }
    }

    fn set_slot<'gc>(a: &Agent<'gc, '_>, f: JsObject<'gc>, i: usize, v: Value<'gc>) {
        if let ObjectKind::NativeClosure(c) = &mut f.borrow_mut(a.mc).kind {
            c.slots[i] = v;
        }
    }

    /// GetCapabilitiesExecutor: each can be set only once.
    fn capability_executor<'gc>(a: &mut Agent<'gc, '_>, f: JsObject<'gc>, _: Value<'gc>, args: &[Value<'gc>]) -> Result<Value<'gc>, Error> {
        let s = Self::slots(f);
        if s[0] != Value::Undefined || s[1] != Value::Undefined {
            return Err(a.throw(ErrorKind::Type, "Promise executor has already been invoked with non-undefined arguments"));
        }
        Self::set_slot(a, f, 0, args.at(0));
        Self::set_slot(a, f, 1, args.at(1));
        Ok(Value::Undefined)
    }

    /// NewPromiseCapability(C).
    fn new_capability<'gc>(a: &mut Agent<'gc, '_>, c: Value<'gc>) -> Result<Capability<'gc>, Error> {
        if Self::is_intrinsic(a, c) {
            let p = a.new_promise();
            let (resolve, reject) = a.promise_resolvers(p);
            return Ok(Capability { promise: p.into(), resolve: resolve.into(), reject: reject.into() });
        }
        if !a.is_constructor(c) {
            return Err(a.throw(ErrorKind::Type, "Promise capability constructor is not a constructor"));
        }
        let executor = Self::closure(a, Self::capability_executor, alloc::vec![Value::Undefined, Value::Undefined], 2);
        let promise = a.construct(c, &[executor.into()], c)?;
        let s = Self::slots(executor);
        if !a.is_callable(s[0]) || !a.is_callable(s[1]) {
            return Err(a.throw(ErrorKind::Type, "Promise resolve or reject function is not callable"));
        }
        Ok(Capability { promise, resolve: s[0], reject: s[1] })
    }

    /// PromiseResolve(C, x).
    fn resolve_with<'gc>(a: &mut Agent<'gc, '_>, c: Value<'gc>, x: Value<'gc>) -> Result<Value<'gc>, Error> {
        if Agent::is_promise(x) {
            let xc = a.get_named(x, "constructor")?;
            if xc.same_value(c) {
                return Ok(x);
            }
        }
        let cap = Self::new_capability(a, c)?;
        a.call(cap.resolve, Value::Undefined, &[x])?;
        Ok(cap.promise)
    }

    fn construct<'gc>(a: &mut Agent<'gc, '_>, _: Value<'gc>, args: &[Value<'gc>]) -> Result<Value<'gc>, Error> {
        let executor = args.at(0);
        if a.new_target == Value::Undefined {
            return Err(a.throw(ErrorKind::Type, "Promise constructor cannot be invoked without 'new'"));
        }
        if !a.is_callable(executor) {
            let what = a.describe(executor);
            return Err(a.throw(ErrorKind::Type, &alloc::format!("Promise resolver {what} is not a function")));
        }
        let proto = a.proto_from_ctor(a.new_target, a.realm.promise_proto)?;
        let p = a.new_promise();
        p.borrow_mut(a.mc).proto = Some(proto);
        let (resolve, reject) = a.promise_resolvers(p);
        match a.call(executor, Value::Undefined, &[resolve.into(), reject.into()]) {
            Ok(_) => {}
            Err(Error::Exception(g)) => {
                let reason = a.local(&g);
                a.call(reject.into(), Value::Undefined, &[reason])?;
            }
            Err(e) => return Err(e),
        }
        Ok(p.into())
    }

    fn this_object<'gc>(a: &Agent<'gc, '_>, this: Value<'gc>) -> Result<Value<'gc>, Error> {
        match this.kind() {
            ValueKind::Object(_) => Ok(this),
            _ => Err(a.throw(ErrorKind::Type, "Promise method called on a non-object")),
        }
    }

    fn resolve<'gc>(a: &mut Agent<'gc, '_>, this: Value<'gc>, args: &[Value<'gc>]) -> Result<Value<'gc>, Error> {
        let c = Self::this_object(a, this)?;
        Self::resolve_with(a, c, args.at(0))
    }

    fn reject<'gc>(a: &mut Agent<'gc, '_>, this: Value<'gc>, args: &[Value<'gc>]) -> Result<Value<'gc>, Error> {
        let cap = Self::new_capability(a, this)?;
        a.call(cap.reject, Value::Undefined, &[args.at(0)])?;
        Ok(cap.promise)
    }

    fn with_resolvers<'gc>(a: &mut Agent<'gc, '_>, this: Value<'gc>, _: &[Value<'gc>]) -> Result<Value<'gc>, Error> {
        let cap = Self::new_capability(a, this)?;
        let o = a.object();
        a.create_data_property_or_throw(o, Key::Str("promise"), cap.promise)?;
        a.create_data_property_or_throw(o, Key::Str("resolve"), cap.resolve)?;
        a.create_data_property_or_throw(o, Key::Str("reject"), cap.reject)?;
        Ok(o.into())
    }

    /// `Promise.try(f, ...args)`.
    fn try_<'gc>(a: &mut Agent<'gc, '_>, this: Value<'gc>, args: &[Value<'gc>]) -> Result<Value<'gc>, Error> {
        let c = Self::this_object(a, this)?;
        let cap = Self::new_capability(a, c)?;
        match a.call(args.at(0), Value::Undefined, args.get(1..).unwrap_or(&[])) {
            Ok(v) => a.call(cap.resolve, Value::Undefined, &[v])?,
            Err(Error::Exception(g)) => {
                let reason = a.local(&g);
                a.call(cap.reject, Value::Undefined, &[reason])?
            }
            Err(e) => return Err(e),
        };
        Ok(cap.promise)
    }

    fn this_promise<'gc>(a: &Agent<'gc, '_>, this: Value<'gc>) -> Result<JsObject<'gc>, Error> {
        match this.kind() {
            ValueKind::Object(o) if Agent::is_promise(this) => Ok(o),
            _ => Err(a.throw(ErrorKind::Type, "Promise.prototype method called on a non-promise")),
        }
    }

    fn then<'gc>(a: &mut Agent<'gc, '_>, this: Value<'gc>, args: &[Value<'gc>]) -> Result<Value<'gc>, Error> {
        let p = Self::this_promise(a, this)?;
        let c = Buffers::species_constructor(a, this, a.realm.promise_ctor.into())?;
        let cap = Self::new_capability(a, c)?;
        a.perform_then(p, Reaction::Then { on_fulfilled: args.at(0), on_rejected: args.at(1), capability: Some(cap) });
        Ok(cap.promise)
    }

    fn catch<'gc>(a: &mut Agent<'gc, '_>, this: Value<'gc>, args: &[Value<'gc>]) -> Result<Value<'gc>, Error> {
        let then = a.get_named(this, "then")?;
        a.call(then, this, &[Value::Undefined, args.at(0)])
    }

    /// `finally(f)`: `then(thenFinally, catchFinally)`; both call `f()` first, then pass on the original value /
    /// reason.
    fn finally<'gc>(a: &mut Agent<'gc, '_>, this: Value<'gc>, args: &[Value<'gc>]) -> Result<Value<'gc>, Error> {
        let p = Self::this_object(a, this)?;
        let c = Buffers::species_constructor(a, p, a.realm.promise_ctor.into())?;
        let f = args.at(0);
        let (then_f, catch_f): (Value<'gc>, Value<'gc>) = if a.is_callable(f) {
            (Self::closure(a, Self::then_finally, alloc::vec![f, c], 1).into(), Self::closure(a, Self::catch_finally, alloc::vec![f, c], 1).into())
        } else {
            (f, f)
        };
        let then = a.get_named(p, "then")?;
        a.call(then, p, &[then_f, catch_f])
    }

    fn finally_step<'gc>(a: &mut Agent<'gc, '_>, f: JsObject<'gc>, v: Value<'gc>, rethrow: bool) -> Result<Value<'gc>, Error> {
        let s = Self::slots(f);
        let result = a.call(s[0], Value::Undefined, &[])?;
        let p = Self::resolve_with(a, s[1], result)?;
        let thunk = Self::closure(a, if rethrow { Self::thrower } else { Self::value_thunk }, alloc::vec![v], 0);
        let then = a.get_named(p, "then")?;
        a.call(then, p, &[thunk.into()])
    }

    fn then_finally<'gc>(a: &mut Agent<'gc, '_>, f: JsObject<'gc>, _: Value<'gc>, args: &[Value<'gc>]) -> Result<Value<'gc>, Error> {
        Self::finally_step(a, f, args.at(0), false)
    }

    fn catch_finally<'gc>(a: &mut Agent<'gc, '_>, f: JsObject<'gc>, _: Value<'gc>, args: &[Value<'gc>]) -> Result<Value<'gc>, Error> {
        Self::finally_step(a, f, args.at(0), true)
    }

    fn value_thunk<'gc>(_: &mut Agent<'gc, '_>, f: JsObject<'gc>, _: Value<'gc>, _: &[Value<'gc>]) -> Result<Value<'gc>, Error> {
        Ok(Self::slots(f)[0])
    }

    fn thrower<'gc>(a: &mut Agent<'gc, '_>, f: JsObject<'gc>, _: Value<'gc>, _: &[Value<'gc>]) -> Result<Value<'gc>, Error> {
        Err(a.throw_value(Self::slots(f)[0]))
    }

    fn all<'gc>(a: &mut Agent<'gc, '_>, this: Value<'gc>, args: &[Value<'gc>]) -> Result<Value<'gc>, Error> {
        Self::combine(a, this, args.at(0), Combinator::All)
    }

    fn all_settled<'gc>(a: &mut Agent<'gc, '_>, this: Value<'gc>, args: &[Value<'gc>]) -> Result<Value<'gc>, Error> {
        Self::combine(a, this, args.at(0), Combinator::AllSettled)
    }

    fn any<'gc>(a: &mut Agent<'gc, '_>, this: Value<'gc>, args: &[Value<'gc>]) -> Result<Value<'gc>, Error> {
        Self::combine(a, this, args.at(0), Combinator::Any)
    }

    fn race<'gc>(a: &mut Agent<'gc, '_>, this: Value<'gc>, args: &[Value<'gc>]) -> Result<Value<'gc>, Error> {
        Self::combine(a, this, args.at(0), Combinator::Race)
    }

    /// IfAbruptRejectPromise: hand the exception to the capability record's reject and return the result Promise.
    fn reject_on<'gc>(a: &mut Agent<'gc, '_>, cap: &Capability<'gc>, e: Error) -> Result<Value<'gc>, Error> {
        match e {
            Error::Exception(g) => {
                let reason = a.local(&g);
                a.call(cap.reject, Value::Undefined, &[reason])?;
                Ok(cap.promise)
            }
            e => Err(e),
        }
    }

    fn combine<'gc>(a: &mut Agent<'gc, '_>, c: Value<'gc>, iterable: Value<'gc>, how: Combinator) -> Result<Value<'gc>, Error> {
        let cap = Self::new_capability(a, c)?;
        let resolve = match a.get_named(c, "resolve") {
            Ok(f) if a.is_callable(f) => f,
            Ok(_) => {
                let e = a.throw(ErrorKind::Type, "Promise resolve is not a function");
                return Self::reject_on(a, &cap, e);
            }
            Err(e) => return Self::reject_on(a, &cap, e),
        };
        let it = match a.get_iterator(iterable) {
            Ok(it) => it,
            Err(e) => return Self::reject_on(a, &cap, e),
        };
        let mut done = false;
        match Self::perform(a, c, resolve, it, &cap, how, &mut done) {
            Ok(v) => Ok(v),
            Err(e) => {
                let e = if done {
                    e
                } else {
                    let _ = a.iterator_close(it.iterator);
                    e
                };
                Self::reject_on(a, &cap, e)
            }
        }
    }

    fn box_value<'gc>(a: &Agent<'gc, '_>, v: Value<'gc>) -> Value<'gc> {
        a.array(alloc::vec![v]).into()
    }

    fn remaining_add<'gc>(a: &mut Agent<'gc, '_>, remaining: Value<'gc>, d: f64) -> Result<f64, Error> {
        let n = a.get_index(remaining, 0.0)?;
        let ValueKind::Number(n) = n.kind() else { return Ok(0.0) };
        a.set_index(remaining, 0.0, (n + d).into())?;
        Ok(n + d)
    }

    fn perform<'gc>(a: &mut Agent<'gc, '_>, c: Value<'gc>, resolve: Value<'gc>, it: r8framework::IterRecord<'gc>, cap: &Capability<'gc>, how: Combinator, done: &mut bool) -> Result<Value<'gc>, Error> {
        let values = a.array(Vec::new());
        let remaining = Self::box_value(a, 1.0.into());
        let mut index = 0.0;
        loop {
            let next = match a.iterator_step(it) {
                Ok(n) => n,
                Err(e) => {
                    *done = true;
                    return Err(e);
                }
            };
            let Some(v) = next else {
                *done = true;
                if how != Combinator::Race && Self::remaining_add(a, remaining, -1.0)? == 0.0 {
                    Self::finish(a, how, values, cap)?;
                }
                return Ok(cap.promise);
            };
            if how != Combinator::Race {
                a.create_index(values, index, Value::Undefined)?;
            }
            let next_promise = a.call(resolve, c, &[v])?;
            let element = |a: &mut Agent<'gc, '_>, f: r8framework::ClosureFn, settle: Value<'gc>, called: Value<'gc>| -> Value<'gc> {
                Self::closure(a, f, alloc::vec![called, index.into(), values.into(), settle, remaining], 1).into()
            };
            let (on_fulfilled, on_rejected) = match how {
                Combinator::Race => (cap.resolve, cap.reject),
                Combinator::All => {
                    let called = Self::box_value(a, false.into());
                    (element(a, Self::all_element, cap.resolve, called), cap.reject)
                }
                Combinator::AllSettled => {
                    let called = Self::box_value(a, false.into());
                    (element(a, Self::settled_fulfilled, cap.resolve, called), element(a, Self::settled_rejected, cap.resolve, called))
                }
                Combinator::Any => {
                    let called = Self::box_value(a, false.into());
                    (cap.resolve, element(a, Self::any_element, cap.reject, called))
                }
            };
            if how != Combinator::Race {
                Self::remaining_add(a, remaining, 1.0)?;
            }
            let then = a.get_named(next_promise, "then")?;
            a.call(then, next_promise, &[on_fulfilled, on_rejected])?;
            index += 1.0;
        }
    }

    /// Everything is in: All / AllSettled fulfill with an array, Any rejects with an AggregateError.
    fn finish<'gc>(a: &mut Agent<'gc, '_>, how: Combinator, values: JsObject<'gc>, cap: &Capability<'gc>) -> Result<(), Error> {
        let list = a.list_from_array_like(values.into())?;
        let arr = a.array(list);
        if how == Combinator::Any {
            let e = a.error(ErrorKind::Aggregate, "All promises were rejected");
            e.borrow_mut(a.mc).define("errors", Property::hidden(arr.into()));
            a.call(cap.reject, Value::Undefined, &[e.into()])?;
        } else {
            a.call(cap.resolve, Value::Undefined, &[arr.into()])?;
        }
        Ok(())
    }

    /// Common part of element functions: only the first call takes effect; fill slot `index`, settle when the count
    /// reaches 0.
    fn element_step<'gc>(a: &mut Agent<'gc, '_>, f: JsObject<'gc>, v: Value<'gc>, how: Combinator) -> Result<Value<'gc>, Error> {
        let s = Self::slots(f);
        if a.get_index(s[slot::CALLED], 0.0)? == Value::Bool(true) {
            return Ok(Value::Undefined);
        }
        a.set_index(s[slot::CALLED], 0.0, true.into())?;
        let (ValueKind::Number(index), ValueKind::Object(values)) = (s[slot::INDEX].kind(), s[slot::VALUES].kind()) else { return Ok(Value::Undefined) };
        a.create_index(values, index, v)?;
        if Self::remaining_add(a, s[slot::REMAINING], -1.0)? == 0.0 {
            let ValueKind::Object(values) = s[slot::VALUES].kind() else { return Ok(Value::Undefined) };
            let list = a.list_from_array_like(values.into())?;
            let arr = a.array(list);
            if how == Combinator::Any {
                let e = a.error(ErrorKind::Aggregate, "All promises were rejected");
                e.borrow_mut(a.mc).define("errors", Property::hidden(arr.into()));
                return a.call(s[slot::SETTLE], Value::Undefined, &[e.into()]);
            }
            return a.call(s[slot::SETTLE], Value::Undefined, &[arr.into()]);
        }
        Ok(Value::Undefined)
    }

    fn all_element<'gc>(a: &mut Agent<'gc, '_>, f: JsObject<'gc>, _: Value<'gc>, args: &[Value<'gc>]) -> Result<Value<'gc>, Error> {
        Self::element_step(a, f, args.at(0), Combinator::All)
    }

    fn any_element<'gc>(a: &mut Agent<'gc, '_>, f: JsObject<'gc>, _: Value<'gc>, args: &[Value<'gc>]) -> Result<Value<'gc>, Error> {
        Self::element_step(a, f, args.at(0), Combinator::Any)
    }

    fn settled<'gc>(a: &mut Agent<'gc, '_>, f: JsObject<'gc>, v: Value<'gc>, fulfilled: bool) -> Result<Value<'gc>, Error> {
        let s = Self::slots(f);
        if a.get_index(s[slot::CALLED], 0.0)? == Value::Bool(true) {
            return Ok(Value::Undefined);
        }
        let o = a.object();
        let status = a.string(if fulfilled { "fulfilled" } else { "rejected" });
        a.create_data_property_or_throw(o, Key::Str("status"), status.into())?;
        a.create_data_property_or_throw(o, Key::Str(if fulfilled { "value" } else { "reason" }), v)?;
        Self::element_step(a, f, o.into(), Combinator::AllSettled)
    }

    fn settled_fulfilled<'gc>(a: &mut Agent<'gc, '_>, f: JsObject<'gc>, _: Value<'gc>, args: &[Value<'gc>]) -> Result<Value<'gc>, Error> {
        Self::settled(a, f, args.at(0), true)
    }

    fn settled_rejected<'gc>(a: &mut Agent<'gc, '_>, f: JsObject<'gc>, _: Value<'gc>, args: &[Value<'gc>]) -> Result<Value<'gc>, Error> {
        Self::settled(a, f, args.at(0), false)
    }

    fn species<'gc>(_: &mut Agent<'gc, '_>, this: Value<'gc>, _: &[Value<'gc>]) -> Result<Value<'gc>, Error> {
        Ok(this)
    }
}

impl Installer<'_, '_> {
    pub(crate) fn promise(&mut self) {
        let r = self.realm;
        let proto = r.promise_proto;
        let ctor = r.promise_ctor;
        ctor.borrow_mut(self.mc).kind = ObjectKind::Native(r8framework::Native::constructor(Promises::construct));
        r.name_function(self.mc, ctor, "Promise", Some(1));
        ctor.borrow_mut(self.mc).define("prototype", Property { slot: Slot::Data(proto.into()), enumerable: false, writable: false, configurable: false });
        self.set(proto, "constructor", ctor.into());
        self.set(r.global, "Promise", ctor.into());
        let statics: [(&str, r8framework::NativeFn, u32); 8] = [
            ("all", Promises::all, 1),
            ("allSettled", Promises::all_settled, 1),
            ("any", Promises::any, 1),
            ("race", Promises::race, 1),
            ("reject", Promises::reject, 1),
            ("resolve", Promises::resolve, 1),
            ("try", Promises::try_, 1),
            ("withResolvers", Promises::with_resolvers, 0),
        ];
        for (name, f, n) in statics {
            self.method(ctor, name, f, n);
        }
        let species = r.named_native(self.mc, Promises::species, "get [Symbol.species]", 0);
        ctor.borrow_mut(self.mc).define_symbol(r.symbols.species, Property { slot: Slot::Accessor { get: Some(species), set: None }, enumerable: false, writable: false, configurable: true });
        self.method(proto, "then", Promises::then, 2);
        self.method(proto, "catch", Promises::catch, 1);
        self.method(proto, "finally", Promises::finally, 1);
    }
}
