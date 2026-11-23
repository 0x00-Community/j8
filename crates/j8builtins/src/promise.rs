use j8framework::ValueKind;
use alloc::vec::Vec;

use j8framework::{Agent, Capability, Error, ErrorKind, JsObject, Key, NativeClosure, ObjectData, ObjectKind, Property, Reaction, Slot, Value};

use crate::buffer::Buffers;
use crate::{Args, Installer};

struct Promises;

#[derive(Clone, Copy, PartialEq, Eq)]
enum Combinator {
    All,
    AllSettled,
    Any,
    Race,
}

mod slot {
    pub const CALLED: usize = 0;
    pub const INDEX: usize = 1;
    pub const VALUES: usize = 2;
    pub const SETTLE: usize = 3;
    pub const REMAINING: usize = 4;
}

impl Promises {
    fn is_intrinsic<'scope>(a: &Agent<'scope, '_>, c: Value<'scope>) -> bool {
        matches!(c.kind(), ValueKind::Object(o) if j8framework::ScopeLocal::ptr_eq(o, a.realm.promise_ctor))
    }

    fn closure<'scope>(a: &Agent<'scope, '_>, f: j8framework::ClosureFn, slots: Vec<Value<'scope>>, length: u32) -> JsObject<'scope> {
        let o = ObjectData { kind: ObjectKind::NativeClosure(NativeClosure { f, slots, constructor: false }), proto: Some(a.realm.function_proto), ..ObjectData::default() }.alloc(a.mc);
        a.realm.name_function(a.mc, o, "", Some(length));
        o
    }

    fn slots(f: JsObject<'_>) -> Vec<Value<'_>> {
        match &f.borrow().kind {
            ObjectKind::NativeClosure(c) => c.slots.clone(),
            _ => Vec::new(),
        }
    }

    fn set_slot<'scope>(a: &Agent<'scope, '_>, f: JsObject<'scope>, i: usize, v: Value<'scope>) {
        if let ObjectKind::NativeClosure(c) = &mut f.borrow_mut(a.mc).kind {
            c.slots[i] = v;
        }
    }

    fn capability_executor<'scope>(a: &mut Agent<'scope, '_>, f: JsObject<'scope>, _: Value<'scope>, args: &[Value<'scope>]) -> Result<Value<'scope>, Error> {
        let s = Self::slots(f);
        if s[0] != Value::Undefined || s[1] != Value::Undefined {
            return Err(a.throw(ErrorKind::Type, "Promise executor has already been invoked with non-undefined arguments"));
        }
        Self::set_slot(a, f, 0, args.at(0));
        Self::set_slot(a, f, 1, args.at(1));
        Ok(Value::Undefined)
    }

    fn new_capability<'scope>(a: &mut Agent<'scope, '_>, c: Value<'scope>) -> Result<Capability<'scope>, Error> {
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

    fn resolve_with<'scope>(a: &mut Agent<'scope, '_>, c: Value<'scope>, x: Value<'scope>) -> Result<Value<'scope>, Error> {
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

    fn construct<'scope>(a: &mut Agent<'scope, '_>, _: Value<'scope>, args: &[Value<'scope>]) -> Result<Value<'scope>, Error> {
        let new_target = a.new_target;
        let executor = args.at(0);
        if new_target == Value::Undefined {
            return Err(a.throw(ErrorKind::Type, "Promise constructor cannot be invoked without 'new'"));
        }
        if !a.is_callable(executor) {
            let what = a.describe(executor);
            return Err(a.throw(ErrorKind::Type, &alloc::format!("Promise resolver {what} is not a function")));
        }
        let proto = a.proto_from_ctor(new_target, a.realm.promise_proto)?;
        let p = a.new_promise();
        p.borrow_mut(a.mc).set_proto(Some(proto));
        let (resolve, reject) = a.promise_resolvers(p);
        match a.call(executor, Value::Undefined, &[resolve.into(), reject.into()]) {
            Ok(_) => {}
            Err(Error::Exception) => {
                let reason = a.take_exception();
                a.call(reject.into(), Value::Undefined, &[reason])?;
            }
            Err(e) => return Err(e),
        }
        Ok(p.into())
    }

    fn this_object<'scope>(a: &Agent<'scope, '_>, this: Value<'scope>) -> Result<Value<'scope>, Error> {
        match this.kind() {
            ValueKind::Object(_) => Ok(this),
            _ => Err(a.throw(ErrorKind::Type, "Promise method called on a non-object")),
        }
    }

    fn resolve<'scope>(a: &mut Agent<'scope, '_>, this: Value<'scope>, args: &[Value<'scope>]) -> Result<Value<'scope>, Error> {
        let c = Self::this_object(a, this)?;
        Self::resolve_with(a, c, args.at(0))
    }

    fn reject<'scope>(a: &mut Agent<'scope, '_>, this: Value<'scope>, args: &[Value<'scope>]) -> Result<Value<'scope>, Error> {
        let cap = Self::new_capability(a, this)?;
        a.call(cap.reject, Value::Undefined, &[args.at(0)])?;
        Ok(cap.promise)
    }

    fn with_resolvers<'scope>(a: &mut Agent<'scope, '_>, this: Value<'scope>, _: &[Value<'scope>]) -> Result<Value<'scope>, Error> {
        let cap = Self::new_capability(a, this)?;
        let o = a.object();
        a.create_data_property_or_throw(o, Key::Str("promise"), cap.promise)?;
        a.create_data_property_or_throw(o, Key::Str("resolve"), cap.resolve)?;
        a.create_data_property_or_throw(o, Key::Str("reject"), cap.reject)?;
        Ok(o.into())
    }

    fn try_<'scope>(a: &mut Agent<'scope, '_>, this: Value<'scope>, args: &[Value<'scope>]) -> Result<Value<'scope>, Error> {
        let c = Self::this_object(a, this)?;
        let cap = Self::new_capability(a, c)?;
        match a.call(args.at(0), Value::Undefined, args.get(1..).unwrap_or(&[])) {
            Ok(v) => a.call(cap.resolve, Value::Undefined, &[v])?,
            Err(Error::Exception) => {
                let reason = a.take_exception();
                a.call(cap.reject, Value::Undefined, &[reason])?
            }
            Err(e) => return Err(e),
        };
        Ok(cap.promise)
    }

    fn this_promise<'scope>(a: &Agent<'scope, '_>, this: Value<'scope>) -> Result<JsObject<'scope>, Error> {
        match this.kind() {
            ValueKind::Object(o) if Agent::is_promise(this) => Ok(o),
            _ => Err(a.throw(ErrorKind::Type, "Promise.prototype method called on a non-promise")),
        }
    }

    fn then<'scope>(a: &mut Agent<'scope, '_>, this: Value<'scope>, args: &[Value<'scope>]) -> Result<Value<'scope>, Error> {
        let p = Self::this_promise(a, this)?;
        let c = Buffers::species_constructor(a, this, a.realm.promise_ctor.into())?;
        let cap = Self::new_capability(a, c)?;
        a.perform_then(p, Reaction::Then { on_fulfilled: args.at(0), on_rejected: args.at(1), capability: Some(cap) });
        Ok(cap.promise)
    }

    fn catch<'scope>(a: &mut Agent<'scope, '_>, this: Value<'scope>, args: &[Value<'scope>]) -> Result<Value<'scope>, Error> {
        let then = a.get_named(this, "then")?;
        a.call(then, this, &[Value::Undefined, args.at(0)])
    }

    fn finally<'scope>(a: &mut Agent<'scope, '_>, this: Value<'scope>, args: &[Value<'scope>]) -> Result<Value<'scope>, Error> {
        let p = Self::this_object(a, this)?;
        let c = Buffers::species_constructor(a, p, a.realm.promise_ctor.into())?;
        let f = args.at(0);
        let (then_f, catch_f): (Value<'scope>, Value<'scope>) = if a.is_callable(f) {
            (Self::closure(a, Self::then_finally, alloc::vec![f, c], 1).into(), Self::closure(a, Self::catch_finally, alloc::vec![f, c], 1).into())
        } else {
            (f, f)
        };
        let then = a.get_named(p, "then")?;
        a.call(then, p, &[then_f, catch_f])
    }

    fn finally_step<'scope>(a: &mut Agent<'scope, '_>, f: JsObject<'scope>, v: Value<'scope>, rethrow: bool) -> Result<Value<'scope>, Error> {
        let s = Self::slots(f);
        let result = a.call(s[0], Value::Undefined, &[])?;
        let p = Self::resolve_with(a, s[1], result)?;
        let thunk = Self::closure(a, if rethrow { Self::thrower } else { Self::value_thunk }, alloc::vec![v], 0);
        let then = a.get_named(p, "then")?;
        a.call(then, p, &[thunk.into()])
    }

    fn then_finally<'scope>(a: &mut Agent<'scope, '_>, f: JsObject<'scope>, _: Value<'scope>, args: &[Value<'scope>]) -> Result<Value<'scope>, Error> {
        Self::finally_step(a, f, args.at(0), false)
    }

    fn catch_finally<'scope>(a: &mut Agent<'scope, '_>, f: JsObject<'scope>, _: Value<'scope>, args: &[Value<'scope>]) -> Result<Value<'scope>, Error> {
        Self::finally_step(a, f, args.at(0), true)
    }

    fn value_thunk<'scope>(_: &mut Agent<'scope, '_>, f: JsObject<'scope>, _: Value<'scope>, _: &[Value<'scope>]) -> Result<Value<'scope>, Error> {
        Ok(Self::slots(f)[0])
    }

    fn thrower<'scope>(a: &mut Agent<'scope, '_>, f: JsObject<'scope>, _: Value<'scope>, _: &[Value<'scope>]) -> Result<Value<'scope>, Error> {
        Err(a.throw_value(Self::slots(f)[0]))
    }

    fn all<'scope>(a: &mut Agent<'scope, '_>, this: Value<'scope>, args: &[Value<'scope>]) -> Result<Value<'scope>, Error> {
        Self::combine(a, this, args.at(0), Combinator::All)
    }

    fn all_settled<'scope>(a: &mut Agent<'scope, '_>, this: Value<'scope>, args: &[Value<'scope>]) -> Result<Value<'scope>, Error> {
        Self::combine(a, this, args.at(0), Combinator::AllSettled)
    }

    fn any<'scope>(a: &mut Agent<'scope, '_>, this: Value<'scope>, args: &[Value<'scope>]) -> Result<Value<'scope>, Error> {
        Self::combine(a, this, args.at(0), Combinator::Any)
    }

    fn race<'scope>(a: &mut Agent<'scope, '_>, this: Value<'scope>, args: &[Value<'scope>]) -> Result<Value<'scope>, Error> {
        Self::combine(a, this, args.at(0), Combinator::Race)
    }

    fn reject_on<'scope>(a: &mut Agent<'scope, '_>, cap: &Capability<'scope>, e: Error) -> Result<Value<'scope>, Error> {
        match e {
            Error::Exception => {
                let reason = a.take_exception();
                a.call(cap.reject, Value::Undefined, &[reason])?;
                Ok(cap.promise)
            }
            e => Err(e),
        }
    }

    fn combine<'scope>(a: &mut Agent<'scope, '_>, c: Value<'scope>, iterable: Value<'scope>, how: Combinator) -> Result<Value<'scope>, Error> {
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

    fn box_value<'scope>(a: &Agent<'scope, '_>, v: Value<'scope>) -> Value<'scope> {
        a.array(alloc::vec![v]).into()
    }

    fn remaining_add<'scope>(a: &mut Agent<'scope, '_>, remaining: Value<'scope>, d: f64) -> Result<f64, Error> {
        let n = a.get_index(remaining, 0.0)?;
        let Some(n) = n.as_number() else { return Ok(0.0) };
        a.set_index(remaining, 0.0, (n + d).into())?;
        Ok(n + d)
    }

    fn perform<'scope>(a: &mut Agent<'scope, '_>, c: Value<'scope>, resolve: Value<'scope>, it: j8framework::IterRecord<'scope>, cap: &Capability<'scope>, how: Combinator, done: &mut bool) -> Result<Value<'scope>, Error> {
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
            let element = |a: &mut Agent<'scope, '_>, f: j8framework::ClosureFn, settle: Value<'scope>, called: Value<'scope>| -> Value<'scope> {
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

    fn finish<'scope>(a: &mut Agent<'scope, '_>, how: Combinator, values: JsObject<'scope>, cap: &Capability<'scope>) -> Result<(), Error> {
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

    fn element_step<'scope>(a: &mut Agent<'scope, '_>, f: JsObject<'scope>, v: Value<'scope>, how: Combinator) -> Result<Value<'scope>, Error> {
        let s = Self::slots(f);
        if a.get_index(s[slot::CALLED], 0.0)? == Value::Bool(true) {
            return Ok(Value::Undefined);
        }
        a.set_index(s[slot::CALLED], 0.0, true.into())?;
        let (ValueKind::Number(index), ValueKind::Object(values)) = (s[slot::INDEX].kind(), s[slot::VALUES].kind()) else { return Ok(Value::Undefined) };
        a.create_index(values, index, v)?;
        if Self::remaining_add(a, s[slot::REMAINING], -1.0)? == 0.0 {
            let Some(values) = s[slot::VALUES].as_object() else { return Ok(Value::Undefined) };
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

    fn all_element<'scope>(a: &mut Agent<'scope, '_>, f: JsObject<'scope>, _: Value<'scope>, args: &[Value<'scope>]) -> Result<Value<'scope>, Error> {
        Self::element_step(a, f, args.at(0), Combinator::All)
    }

    fn any_element<'scope>(a: &mut Agent<'scope, '_>, f: JsObject<'scope>, _: Value<'scope>, args: &[Value<'scope>]) -> Result<Value<'scope>, Error> {
        Self::element_step(a, f, args.at(0), Combinator::Any)
    }

    fn settled<'scope>(a: &mut Agent<'scope, '_>, f: JsObject<'scope>, v: Value<'scope>, fulfilled: bool) -> Result<Value<'scope>, Error> {
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

    fn settled_fulfilled<'scope>(a: &mut Agent<'scope, '_>, f: JsObject<'scope>, _: Value<'scope>, args: &[Value<'scope>]) -> Result<Value<'scope>, Error> {
        Self::settled(a, f, args.at(0), true)
    }

    fn settled_rejected<'scope>(a: &mut Agent<'scope, '_>, f: JsObject<'scope>, _: Value<'scope>, args: &[Value<'scope>]) -> Result<Value<'scope>, Error> {
        Self::settled(a, f, args.at(0), false)
    }

    fn species<'scope>(_: &mut Agent<'scope, '_>, this: Value<'scope>, _: &[Value<'scope>]) -> Result<Value<'scope>, Error> {
        Ok(this)
    }
}

impl Installer<'_, '_> {
    pub(crate) fn promise(&mut self) {
        let r = self.realm;
        let proto = r.promise_proto;
        let ctor = r.promise_ctor;
        ctor.borrow_mut(self.mc).kind = ObjectKind::Native(j8framework::Native::constructor(Promises::construct));
        r.name_function(self.mc, ctor, "Promise", Some(1));
        ctor.borrow_mut(self.mc).define("prototype", Property { slot: Slot::Data(proto.into()), enumerable: false, writable: false, configurable: false });
        self.set(proto, "constructor", ctor.into());
        self.set(r.global, "Promise", ctor.into());
        let statics: [(&str, j8framework::NativeFn, u32); 8] = [
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
