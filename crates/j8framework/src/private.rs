//! Private class members (`#x`): looked up only on the object itself, not along the prototype chain; a TypeError is
//! thrown if absent.
use crate::ValueKind;
use alloc::boxed::Box;

use crate::{Gc};

use crate::{Agent, Error, ErrorKind, JsObject, JsSymbol, Property, Slot, SymbolData, Value};

impl<'gc> Agent<'gc, '_> {
    /// A new private name (its description is `#x`).
    pub fn private_name(&self, description: &str) -> JsSymbol<'gc> {
        self.mc.alloc(SymbolData { description: Some(Box::from(description)) })
    }

    fn private_label(key: JsSymbol<'gc>) -> &'gc str {
        Gc::get(key).description.as_deref().unwrap_or("#")
    }

    fn private_holder(&self, obj: Value<'gc>, key: JsSymbol<'gc>, verb: &str) -> Result<JsObject<'gc>, Error> {
        match obj.kind() {
            ValueKind::Object(o) => Ok(o),
            _ => Err(self.throw(ErrorKind::Type, &alloc::format!("Cannot {verb} private member {} from a non-object", Self::private_label(key)))),
        }
    }

    fn private_lookup(o: JsObject<'gc>, key: JsSymbol<'gc>) -> Option<Property<'gc>> {
        o.borrow().privates.iter().find_map(|&(k, p)| Gc::ptr_eq(k, key).then_some(p))
    }

    /// `obj.#x`.
    pub fn private_get(&mut self, obj: Value<'gc>, key: JsSymbol<'gc>) -> Result<Value<'gc>, Error> {
        let o = self.private_holder(obj, key, "read")?;
        match Self::private_lookup(o, key).map(|p| p.slot) {
            Some(Slot::Data(v)) => Ok(v),
            Some(Slot::Accessor { get: Some(g), .. }) => self.call(g.into(), obj, &[]),
            Some(Slot::Accessor { get: None, .. }) => Err(self.throw(ErrorKind::Type, &alloc::format!("'{}' was defined without a getter", Self::private_label(key)))),
            None => Err(self.throw(ErrorKind::Type, &alloc::format!("Cannot read private member {} from an object whose class did not declare it", Self::private_label(key)))),
        }
    }

    /// `obj.#x = v`: private methods can't be assigned, and an accessor without a setter throws.
    pub fn private_set(&mut self, obj: Value<'gc>, key: JsSymbol<'gc>, value: Value<'gc>) -> Result<(), Error> {
        let o = self.private_holder(obj, key, "write")?;
        match Self::private_lookup(o, key) {
            Some(Property { slot: Slot::Data(_), writable: true, .. }) => {
                let mut d = o.borrow_mut(self.mc);
                if let Some((_, p)) = d.privates.iter_mut().find(|(k, _)| Gc::ptr_eq(*k, key)) {
                    p.slot = Slot::Data(value);
                }
                Ok(())
            }
            Some(Property { slot: Slot::Data(_), .. }) => Err(self.throw(ErrorKind::Type, &alloc::format!("Private method {} is not writable", Self::private_label(key)))),
            Some(Property { slot: Slot::Accessor { set: Some(s), .. }, .. }) => self.call(s.into(), obj, &[value]).map(drop),
            Some(Property { slot: Slot::Accessor { set: None, .. }, .. }) => Err(self.throw(ErrorKind::Type, &alloc::format!("'{}' was defined without a setter", Self::private_label(key)))),
            None => Err(self.throw(ErrorKind::Type, &alloc::format!("Cannot write private member {} to an object whose class did not declare it", Self::private_label(key)))),
        }
    }

    /// Field initialization / method installation: throws a TypeError if already present; merges with the other half of
    /// an existing accessor.
    pub fn private_define(&mut self, obj: Value<'gc>, key: JsSymbol<'gc>, prop: Property<'gc>) -> Result<(), Error> {
        let o = self.private_holder(obj, key, "define")?;
        let existing = Self::private_lookup(o, key);
        if existing.is_none() && o.borrow().non_extensible() {
            return Err(self.throw(ErrorKind::Type, &alloc::format!("Cannot define private field {} on a non-extensible object", Self::private_label(key))));
        }
        let merged = match (existing, prop.slot) {
            (None, _) => prop,
            (Some(Property { slot: Slot::Accessor { get, set }, .. }), Slot::Accessor { get: g2, set: s2 }) if (get.is_none() || g2.is_none()) && (set.is_none() || s2.is_none()) => {
                Property { slot: Slot::Accessor { get: get.or(g2), set: set.or(s2) }, ..prop }
            }
            _ => return Err(self.throw(ErrorKind::Type, &alloc::format!("Cannot initialize {} twice on the same object", Self::private_label(key)))),
        };
        let mut d = o.borrow_mut(self.mc);
        match d.privates.iter_mut().find(|(k, _)| Gc::ptr_eq(*k, key)) {
            Some((_, p)) => *p = merged,
            None => d.privates.push((key, merged)),
        }
        Ok(())
    }

    /// `#x in obj`: throws a TypeError if `obj` is not an object.
    pub fn private_has(&mut self, obj: Value<'gc>, key: JsSymbol<'gc>) -> Result<bool, Error> {
        match obj.kind() {
            ValueKind::Object(o) => Ok(Self::private_lookup(o, key).is_some()),
            _ => {
                let what = self.describe(obj);
                Err(self.throw(ErrorKind::Type, &alloc::format!("Cannot use 'in' operator to search for '{}' in {what}", Self::private_label(key))))
            }
        }
    }
}
