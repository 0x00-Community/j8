use crate::ValueKind;
use alloc::boxed::Box;

use crate::{ScopeLocal};

use crate::{Agent, Error, ErrorKind, JsObject, JsSymbol, Property, Slot, SymbolData, Value};

impl<'scope> Agent<'scope, '_> {
    pub fn private_key(key: Value<'scope>) -> Option<JsSymbol<'scope>> {
        match key.kind() {
            ValueKind::Symbol(s) if s.private => Some(s),
            _ => None,
        }
    }

    pub fn private_name(&self, description: &str) -> JsSymbol<'scope> {
        self.mc.alloc(SymbolData { description: Some(Box::from(description)), private: true })
    }

    fn private_label(key: JsSymbol<'scope>) -> &'scope str {
        ScopeLocal::get(key).description.as_deref().unwrap_or("#")
    }

    fn private_holder(&self, obj: Value<'scope>, key: JsSymbol<'scope>, verb: &str) -> Result<JsObject<'scope>, Error> {
        match obj.kind() {
            ValueKind::Object(o) => Ok(o),
            _ => Err(self.throw(ErrorKind::Type, &alloc::format!("Cannot {verb} private member {} from a non-object", Self::private_label(key)))),
        }
    }

    fn private_lookup(o: JsObject<'scope>, key: JsSymbol<'scope>) -> Option<Property<'scope>> {
        o.borrow().privates.iter().find_map(|&(k, p)| ScopeLocal::ptr_eq(k, key).then_some(p))
    }

    pub fn private_get(&mut self, obj: Value<'scope>, key: JsSymbol<'scope>) -> Result<Value<'scope>, Error> {
        let o = self.private_holder(obj, key, "read")?;
        match Self::private_lookup(o, key).map(|p| p.slot) {
            Some(Slot::Data(v)) => Ok(v),
            Some(Slot::Accessor { get: Some(g), .. }) => self.call(g.into(), obj, &[]),
            Some(Slot::Accessor { get: None, .. }) => Err(self.throw(ErrorKind::Type, &alloc::format!("'{}' was defined without a getter", Self::private_label(key)))),
            None => Err(self.throw(ErrorKind::Type, &alloc::format!("Cannot read private member {} from an object whose class did not declare it", Self::private_label(key)))),
        }
    }

    pub fn private_set(&mut self, obj: Value<'scope>, key: JsSymbol<'scope>, value: Value<'scope>) -> Result<(), Error> {
        let o = self.private_holder(obj, key, "write")?;
        match Self::private_lookup(o, key) {
            Some(Property { slot: Slot::Data(_), writable: true, .. }) => {
                let mut d = o.borrow_mut(self.mc);
                if let Some((_, p)) = d.privates.iter_mut().find(|(k, _)| ScopeLocal::ptr_eq(*k, key)) {
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

    pub fn private_define(&mut self, obj: Value<'scope>, key: JsSymbol<'scope>, prop: Property<'scope>) -> Result<(), Error> {
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
        match d.privates.iter_mut().find(|(k, _)| ScopeLocal::ptr_eq(*k, key)) {
            Some((_, p)) => *p = merged,
            None => d.privates.push((key, merged)),
        }
        Ok(())
    }

    pub fn private_has(&mut self, obj: Value<'scope>, key: JsSymbol<'scope>) -> Result<bool, Error> {
        match obj.kind() {
            ValueKind::Object(o) => Ok(Self::private_lookup(o, key).is_some()),
            _ => {
                let what = self.describe(obj);
                Err(self.throw(ErrorKind::Type, &alloc::format!("Cannot use 'in' operator to search for '{}' in {what}", Self::private_label(key))))
            }
        }
    }
}
