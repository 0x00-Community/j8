use j8framework::ValueKind;
use j8framework::{Agent, Error, ErrorKind, JsObject, ObjectKind, Value};

use crate::{Args, Installer};

/// `WeakRef` and `FinalizationRegistry` (no weak references yet: targets stay alive forever and cleanup callbacks are
/// never called).
struct Weaks;

impl Weaks {
    fn weak_ref<'gc>(a: &mut Agent<'gc, '_>, _: Value<'gc>, args: &[Value<'gc>]) -> Result<Value<'gc>, Error> {
        if a.new_target == Value::Undefined {
            return Err(a.throw(ErrorKind::Type, "Constructor WeakRef requires 'new'"));
        }
        let target = args.at(0);
        if !a.can_be_held_weakly(target) {
            return Err(a.throw(ErrorKind::Type, "WeakRef: target must be an object or a non-registered symbol"));
        }
        Ok(a.create_from_ctor(a.realm.weak_ref_proto, ObjectKind::WeakRef(target))?.into())
    }

    fn deref<'gc>(a: &mut Agent<'gc, '_>, this: Value<'gc>, _: &[Value<'gc>]) -> Result<Value<'gc>, Error> {
        match this.kind() {
            ValueKind::Object(o) => match o.borrow().kind {
                ObjectKind::WeakRef(t) => Ok(t),
                _ => Err(a.throw(ErrorKind::Type, "WeakRef.prototype.deref called on an incompatible receiver")),
            },
            _ => Err(a.throw(ErrorKind::Type, "WeakRef.prototype.deref called on an incompatible receiver")),
        }
    }

    fn registry<'gc>(a: &mut Agent<'gc, '_>, _: Value<'gc>, args: &[Value<'gc>]) -> Result<Value<'gc>, Error> {
        if a.new_target == Value::Undefined {
            return Err(a.throw(ErrorKind::Type, "Constructor FinalizationRegistry requires 'new'"));
        }
        let cleanup = args.at(0);
        if !a.is_callable(cleanup) {
            return Err(a.throw(ErrorKind::Type, "FinalizationRegistry: cleanup must be callable"));
        }
        Ok(a.create_from_ctor(a.realm.finalization_registry_proto, ObjectKind::FinalizationRegistry { cleanup, cells: alloc::vec::Vec::new() })?.into())
    }

    fn this_registry<'gc>(a: &Agent<'gc, '_>, this: Value<'gc>) -> Result<JsObject<'gc>, Error> {
        match this.kind() {
            ValueKind::Object(o) if matches!(o.borrow().kind, ObjectKind::FinalizationRegistry { .. }) => Ok(o),
            _ => Err(a.throw(ErrorKind::Type, "FinalizationRegistry method called on an incompatible receiver")),
        }
    }

    fn register<'gc>(a: &mut Agent<'gc, '_>, this: Value<'gc>, args: &[Value<'gc>]) -> Result<Value<'gc>, Error> {
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

    fn unregister<'gc>(a: &mut Agent<'gc, '_>, this: Value<'gc>, args: &[Value<'gc>]) -> Result<Value<'gc>, Error> {
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
        let r = self.realm;
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
