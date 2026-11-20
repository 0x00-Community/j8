//! Classes: the heritage of `class … extends`, `super` property access, the names of computed members.
use crate::ValueKind;

use crate::{Agent, Error, ErrorKind, JsObject, Key, PropertyKey, Value};

impl<'gc> Agent<'gc, '_> {
    /// ClassDefinitionEvaluation's heritage: the prototype's parent and the constructor's parent. `superclass` is
    /// `Uninit` (the hole) without `extends`.
    pub fn class_heritage(&mut self, superclass: Value<'gc>) -> Result<(Option<JsObject<'gc>>, JsObject<'gc>), Error> {
        match superclass.kind() {
            ValueKind::Uninit => Ok((Some(self.realm.object_proto), self.realm.function_proto)),
            ValueKind::Null => Ok((None, self.realm.function_proto)),
            ValueKind::Object(s) if self.is_constructor(superclass) => match self.get_named(superclass, "prototype")?.kind() {
                ValueKind::Object(p) => Ok((Some(p), s)),
                ValueKind::Null => Ok((None, s)),
                p => {
                    let what = self.describe(p.into());
                    Err(self.throw(ErrorKind::Type, &alloc::format!("Class extends value does not have valid prototype property {what}")))
                }
            },
            _ => {
                let what = self.describe(superclass);
                Err(self.throw(ErrorKind::Type, &alloc::format!("Class extends value {what} is not a constructor or null")))
            }
        }
    }

    /// Where `super.x` looks: the home object's prototype
    fn super_base(&mut self, home: Value<'gc>, key: &PropertyKey<'gc>) -> Result<JsObject<'gc>, Error> {
        let base = match home.as_object() {
            Some(h) => self.get_prototype_of(h)?,
            None => None,
        };
        base.ok_or_else(|| {
            let what = match key {
                PropertyKey::String(s) => alloc::string::String::from(&***s),
                PropertyKey::Symbol(s) => alloc::string::String::from(&**self.symbol_descriptive(*s)),
            };
            self.throw(ErrorKind::Type, &alloc::format!("Cannot read properties of null (reading '{what}')"))
        })
    }

    /// `super[key]` with `this` being `receiver`
    pub fn super_get(&mut self, home: Value<'gc>, key: Value<'gc>, receiver: Value<'gc>) -> Result<Value<'gc>, Error> {
        let k = self.to_property_key(key)?;
        let base = self.super_base(home, &k)?;
        match k {
            PropertyKey::String(s) => self.get_key(base.into(), Key::Str(&s), receiver),
            PropertyKey::Symbol(s) => self.get_key(base.into(), Key::Sym(s), receiver),
        }
    }

    /// `super[key] = value` (class code is strict: a failed assignment throws)
    pub fn super_set(&mut self, home: Value<'gc>, key: Value<'gc>, value: Value<'gc>, receiver: Value<'gc>) -> Result<(), Error> {
        let k = self.to_property_key(key)?;
        let base = self.super_base(home, &k)?;
        let done = match k {
            PropertyKey::String(s) => self.set_key(base.into(), Key::Str(&s), value, receiver)?,
            PropertyKey::Symbol(s) => self.set_key(base.into(), Key::Sym(s), value, receiver)?,
        };
        if done {
            return Ok(());
        }
        let what = self.describe(key);
        Err(self.throw(ErrorKind::Type, &alloc::format!("Cannot assign to read only property {what} of object")))
    }

    /// SetFunctionName for a computed key: the key's text (a symbol's is `[description]`), after `get ` / `set `
    pub fn set_function_name(&mut self, f: JsObject<'gc>, key: Value<'gc>, prefix: Option<&str>) -> Result<(), Error> {
        let name = match self.to_property_key(key)? {
            PropertyKey::String(s) => alloc::string::String::from(&**s),
            PropertyKey::Symbol(s) => match s.description.as_deref() {
                Some(d) if s.private => alloc::string::String::from(d),
                Some(d) => alloc::format!("[{d}]"),
                None => alloc::string::String::new(),
            },
        };
        let name = match prefix {
            Some(p) => alloc::format!("{p} {name}"),
            None => name,
        };
        let realm = self.realm;
        realm.name_function(self.mc, f, &name, None);
        Ok(())
    }
}
