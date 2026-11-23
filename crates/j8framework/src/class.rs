use crate::ValueKind;

use crate::{Agent, Error, ErrorKind, JsObject, Key, PropertyKey, Value};

impl<'scope> Agent<'scope, '_> {
    pub fn class_heritage(&mut self, superclass: Value<'scope>) -> Result<(Option<JsObject<'scope>>, JsObject<'scope>), Error> {
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

    fn super_proto(&mut self, home: Value<'scope>) -> Result<Option<JsObject<'scope>>, Error> {
        match home.as_object() {
            Some(h) => self.get_prototype_of(h),
            None => Ok(None),
        }
    }

    fn super_base(&mut self, base: Option<JsObject<'scope>>, key: &PropertyKey<'scope>) -> Result<JsObject<'scope>, Error> {
        base.ok_or_else(|| {
            let what = match key {
                PropertyKey::String(s) => alloc::string::String::from(&***s),
                PropertyKey::Symbol(s) => alloc::string::String::from(&**self.symbol_descriptive(*s)),
            };
            self.throw(ErrorKind::Type, &alloc::format!("Cannot read properties of null (reading '{what}')"))
        })
    }

    pub fn super_get(&mut self, home: Value<'scope>, key: Value<'scope>, receiver: Value<'scope>) -> Result<Value<'scope>, Error> {
        let proto = self.super_proto(home)?;
        let k = self.to_property_key(key)?;
        let base = self.super_base(proto, &k)?;
        match k {
            PropertyKey::String(s) => self.get_key(base.into(), Key::Str(&s), receiver),
            PropertyKey::Symbol(s) => self.get_key(base.into(), Key::Sym(s), receiver),
        }
    }

    pub fn super_set(&mut self, home: Value<'scope>, key: Value<'scope>, value: Value<'scope>, receiver: Value<'scope>) -> Result<(), Error> {
        let proto = self.super_proto(home)?;
        let k = self.to_property_key(key)?;
        let base = self.super_base(proto, &k)?;
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

    pub fn set_function_name(&mut self, f: JsObject<'scope>, key: Value<'scope>, prefix: Option<&str>) -> Result<(), Error> {
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
