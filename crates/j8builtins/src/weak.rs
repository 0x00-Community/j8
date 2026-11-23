use j8framework::ValueKind;
use j8framework::{Agent, Error, ErrorKind, JsObject, ObjectKind, Value};

use crate::{Args, Installer};

struct Weaks;

impl Weaks {
    fn weak_ref<'scope>(a: &mut Agent<'scope, '_>, _: Value<'scope>, args: &[Value<'scope>]) -> Result<Value<'scope>, Error> {
        let new_target = a.new_target;
        if new_target == Value::Undefined {
            return Err(a.throw(ErrorKind::Type, "Constructor WeakRef requires 'new'"));
        }
        let target = args.at(0);
        if !a.can_be_held_weakly(target) {
            return Err(a.throw(ErrorKind::Type, "WeakRef: target must be an object or a non-registered symbol"));
        }
        Ok(a.create_from_ctor(new_target, a.context.weak_ref_proto, ObjectKind::WeakRef(target))?.into())
    }

    fn deref<'scope>(a: &mut Agent<'scope, '_>, this: Value<'scope>, _: &[Value<'scope>]) -> Result<Value<'scope>, Error> {
        match this.kind() {
            ValueKind::Object(o) => match o.borrow().kind {
                ObjectKind::WeakRef(t) => Ok(t),
                _ => Err(a.throw(ErrorKind::Type, "WeakRef.prototype.deref called on an incompatible receiver")),
            },
            _ => Err(a.throw(ErrorKind::Type, "WeakRef.prototype.deref called on an incompatible receiver")),
        }
    }

    fn registry<'scope>(a: &mut Agent<'scope, '_>, _: Value<'scope>, args: &[Value<'scope>]) -> Result<Value<'scope>, Error> {
        let new_target = a.new_target;
        if new_target == Value::Undefined {
            return Err(a.throw(ErrorKind::Type, "Constructor FinalizationRegistry requires 'new'"));
        }
        let cleanup = args.at(0);
        if !a.is_callable(cleanup) {
            return Err(a.throw(ErrorKind::Type, "FinalizationRegistry: cleanup must be callable"));
        }
        Ok(a.create_from_ctor(new_target, a.context.finalization_registry_proto, ObjectKind::FinalizationRegistry { cleanup, cells: alloc::vec::Vec::new() })?.into())
    }

    fn this_registry<'scope>(a: &Agent<'scope, '_>, this: Value<'scope>) -> Result<JsObject<'scope>, Error> {
        match this.kind() {
            ValueKind::Object(o) if matches!(o.borrow().kind, ObjectKind::FinalizationRegistry { .. }) => Ok(o),
            _ => Err(a.throw(ErrorKind::Type, "FinalizationRegistry method called on an incompatible receiver")),
        }
    }

    fn register<'scope>(a: &mut Agent<'scope, '_>, this: Value<'scope>, args: &[Value<'scope>]) -> Result<Value<'scope>, Error> {
        let o = Self::this_registry(a, this)?;
        let (target, held, token) = (args.at(0), args.at(1), args.at(2));
        if !a.can_be_held_weakly(target) {
            return Err(a.throw(ErrorKind::Type, "FinalizationRegistry.prototype.register: invalid target"));
        }
        if target.same_value(held) {
            return Err(a.throw(ErrorKind::Type, "FinalizationRegistry.prototype.register: target and holdings must not be same"));
        }
        if token != Value::Undefined && !a.can_be_held_weakly(token) {
            return Err(a.throw(ErrorKind::Type, "FinalizationRegistry.prototype.register: invalid unregister token"));
        }
        if let ObjectKind::FinalizationRegistry { cells, .. } = &mut o.borrow_mut(a.mc).kind {
            cells.push((target, held, token));
        }
        Ok(Value::Undefined)
    }

    fn unregister<'scope>(a: &mut Agent<'scope, '_>, this: Value<'scope>, args: &[Value<'scope>]) -> Result<Value<'scope>, Error> {
        let o = Self::this_registry(a, this)?;
        let token = args.at(0);
        if !a.can_be_held_weakly(token) {
            return Err(a.throw(ErrorKind::Type, "FinalizationRegistry.prototype.unregister: invalid unregister token"));
        }
        let mut removed = false;
        if let ObjectKind::FinalizationRegistry { cells, .. } = &mut o.borrow_mut(a.mc).kind {
            let before = cells.len();
            cells.retain(|(_, _, t)| !t.same_value(token));
            removed = cells.len() != before;
        }
        Ok(removed.into())
    }
}

impl Installer<'_, '_> {
    pub(crate) fn weaks(&mut self) {
        let r = self.context;
        let wr = r.weak_ref_proto;
        self.constructor("WeakRef", Weaks::weak_ref, wr, 1);
        self.method(wr, "deref", Weaks::deref, 0);
        let fr = r.finalization_registry_proto;
        self.constructor("FinalizationRegistry", Weaks::registry, fr, 1);
        self.method(fr, "register", Weaks::register, 2);
        self.method(fr, "unregister", Weaks::unregister, 1);
        for (o, tag) in [(wr, "WeakRef"), (fr, "FinalizationRegistry")] {
            let v = self.text(tag);
            o.borrow_mut(self.mc).define_symbol(r.symbols.to_string_tag, j8framework::Property { slot: j8framework::Slot::Data(v), enumerable: false, writable: false, configurable: true });
        }
    }
}
