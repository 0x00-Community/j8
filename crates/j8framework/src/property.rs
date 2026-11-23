use crate::ValueKind;
use alloc::format;
use alloc::vec::Vec;

use crate::{ScopeLocal};

use crate::ops::index_of_str;
use crate::{Agent, Error, ErrorKind, JsObject, JsString, JsSymbol, ObjectKind, Property, Slot, Value};

#[derive(Clone, Copy, Debug)]
pub enum Key<'k, 'scope> {
    Str(&'k str),
    Sym(JsSymbol<'scope>),
}

impl<'k> From<&'k str> for Key<'k, '_> {
    fn from(s: &'k str) -> Self {
        Key::Str(s)
    }
}

#[derive(Clone, Copy, Debug)]
pub enum PropertyKey<'scope> {
    String(JsString<'scope>),
    Symbol(JsSymbol<'scope>),
}

impl<'scope> PropertyKey<'scope> {
    pub fn key(&self) -> Key<'_, 'scope> {
        match self {
            PropertyKey::String(s) => Key::Str(s),
            PropertyKey::Symbol(s) => Key::Sym(*s),
        }
    }
}

impl<'scope> From<PropertyKey<'scope>> for Value<'scope> {
    fn from(k: PropertyKey<'scope>) -> Self {
        match k {
            PropertyKey::String(s) => Value::String(s),
            PropertyKey::Symbol(s) => Value::Symbol(s),
        }
    }
}

#[derive(Clone, Copy, Debug, Default)]
pub struct Descriptor<'scope> {
    pub value: Option<Value<'scope>>,
    pub writable: Option<bool>,
    pub get: Option<Option<JsObject<'scope>>>,
    pub set: Option<Option<JsObject<'scope>>>,
    pub enumerable: Option<bool>,
    pub configurable: Option<bool>,
}

impl<'scope> Descriptor<'scope> {
    pub fn is_accessor(&self) -> bool {
        self.get.is_some() || self.set.is_some()
    }

    pub fn is_data(&self) -> bool {
        self.value.is_some() || self.writable.is_some()
    }

    pub fn is_generic(&self) -> bool {
        !self.is_accessor() && !self.is_data()
    }

    pub fn value(v: Value<'scope>) -> Self {
        Self { value: Some(v), ..Self::default() }
    }

    pub fn complete(&self) -> Property<'scope> {
        let slot = if self.is_accessor() { Slot::Accessor { get: self.get.flatten(), set: self.set.flatten() } } else { Slot::Data(self.value.unwrap_or_default()) };
        Property { slot, enumerable: self.enumerable.unwrap_or(false), writable: self.writable.unwrap_or(false), configurable: self.configurable.unwrap_or(false) }
    }
}

impl<'scope> From<Property<'scope>> for Descriptor<'scope> {
    fn from(p: Property<'scope>) -> Self {
        let base = Self { enumerable: Some(p.enumerable), configurable: Some(p.configurable), ..Self::default() };
        match p.slot {
            Slot::Data(v) => Self { value: Some(v), writable: Some(p.writable), ..base },
            Slot::Accessor { get, set } => Self { get: Some(get), set: Some(set), ..base },
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Integrity {
    Sealed,
    Frozen,
}

fn same_object<'scope>(a: Option<JsObject<'scope>>, b: Option<JsObject<'scope>>) -> bool {
    match (a, b) {
        (Some(x), Some(y)) => ScopeLocal::ptr_eq(x, y),
        (None, None) => true,
        _ => false,
    }
}

pub fn validate<'scope>(extensible: bool, desc: &Descriptor<'scope>, current: Option<Property<'scope>>) -> Option<Property<'scope>> {
    let Some(cur) = current else {
        return extensible.then(|| desc.complete());
    };
    let cur_accessor = matches!(cur.slot, Slot::Accessor { .. });
    if !cur.configurable {
        if desc.configurable == Some(true) || desc.enumerable.is_some_and(|e| e != cur.enumerable) {
            return None;
        }
        if !desc.is_generic() && desc.is_accessor() != cur_accessor {
            return None;
        }
        match cur.slot {
            Slot::Accessor { get, set } => {
                if desc.get.is_some_and(|g| !same_object(g, get)) || desc.set.is_some_and(|s| !same_object(s, set)) {
                    return None;
                }
            }
            Slot::Data(v) if !cur.writable => {
                if desc.writable == Some(true) || desc.value.is_some_and(|x| !x.same_value(v)) {
                    return None;
                }
            }
            Slot::Data(_) => {}
        }
    }
    let mut p = cur;
    if desc.is_data() && cur_accessor {
        p = Property { slot: Slot::Data(Value::Undefined), writable: false, ..cur };
    } else if desc.is_accessor() && !cur_accessor {
        p = Property { slot: Slot::Accessor { get: None, set: None }, writable: false, ..cur };
    }
    if let Some(v) = desc.value {
        p.slot = Slot::Data(v);
    }
    if let Some(w) = desc.writable {
        p.writable = w;
    }
    if let Slot::Accessor { get, set } = &mut p.slot {
        if let Some(g) = desc.get {
            *get = g;
        }
        if let Some(s) = desc.set {
            *set = s;
        }
    }
    if let Some(e) = desc.enumerable {
        p.enumerable = e;
    }
    if let Some(c) = desc.configurable {
        p.configurable = c;
    }
    Some(p)
}

impl<'scope> Agent<'scope, '_> {
    fn string_own(&self, s: JsString<'scope>, key: &str) -> Option<Property<'scope>> {
        let fixed = |v: Value<'scope>, enumerable| Property { slot: Slot::Data(v), enumerable, writable: false, configurable: false };
        if key == "length" {
            return Some(fixed(f64::from(u32::try_from(s.utf16_len()).unwrap_or(u32::MAX)).into(), false));
        }
        let unit = s.code_unit(index_of_str(key)? as usize)?;
        Some(fixed(self.unit_string(unit).into(), true))
    }

    pub fn get_own_property(&mut self, o: JsObject<'scope>, key: Key<'_, 'scope>) -> Result<Option<Property<'scope>>, Error> {
        if let Some(parts) = self.proxy_parts(o) {
            let (t, h) = parts?;
            return self.proxy_get_own_property(t, h, key);
        }
        if let (Some(b), Key::Str(k)) = (Self::namespace_export(o, key), key) {
            return self.namespace_own(b, k);
        }
        if let Some(i) = Self::typed_numeric(o, key) {
            return Ok(self.typed_get(o, i).map(Property::data));
        }
        if o.has_ordinary_properties() {
            return Ok(o.own_slot(self.mc, key));
        }
        if o.is_array() {
            return Ok(match key {
                Key::Str("length") => {
                    let len = f64::from(o.array_length().unwrap_or(0));
                    Some(Property { slot: Slot::Data(len.into()), enumerable: false, writable: !o.length_readonly(), configurable: false })
                }
                _ => o.own_slot(self.mc, key),
            });
        }
        let d = o.borrow();
        Ok(match (&d.kind, key) {
            (ObjectKind::Array, Key::Str("length")) => {
                let len = f64::from(d.array_length().unwrap_or(0));
                Some(Property { slot: Slot::Data(len.into()), enumerable: false, writable: !d.length_readonly(), configurable: false })
            }
            (ObjectKind::Primitive(v), Key::Str(k)) if let ValueKind::String(s) = v.kind() => self.string_own(s, k).or_else(|| d.own_slot(self.mc, key)),
            (ObjectKind::Arguments(Some(map)), Key::Str(k)) => {
                let slot = index_of_str(k).and_then(|i| map.slots.get(i as usize).copied().flatten());
                match (d.own_slot(self.mc, key), slot) {
                    (Some(p), Some(s)) => Some(Property { slot: Slot::Data(map.env.load(usize::from(s))), ..p }),
                    (p, _) => p,
                }
            }
            _ => d.own_slot(self.mc, key),
        })
    }

    pub fn has_own_property(&mut self, o: JsObject<'scope>, key: Key<'_, 'scope>) -> Result<bool, Error> {
        Ok(self.get_own_property(o, key)?.is_some())
    }

    pub fn define_own_property(&mut self, o: JsObject<'scope>, key: Key<'_, 'scope>, desc: Descriptor<'scope>) -> Result<bool, Error> {
        if let Some(parts) = self.proxy_parts(o) {
            let (t, h) = parts?;
            return self.proxy_define_own_property(t, h, key, desc);
        }
        if let (Some(b), Key::Str(k)) = (Self::namespace_export(o, key), key) {
            let Some(cur) = self.namespace_own(b, k)? else { return Ok(false) };
            if desc.configurable == Some(true) || desc.enumerable == Some(false) || desc.is_accessor() || desc.writable == Some(false) {
                return Ok(false);
            }
            return Ok(desc.value.is_none_or(|v| cur.value().is_some_and(|c| c.same_value(v))));
        }
        if let Some(i) = Self::typed_numeric(o, key) {
            if !Self::typed_has(o, i) || desc.configurable == Some(false) || desc.enumerable == Some(false) || desc.is_accessor() || desc.writable == Some(false) {
                return Ok(false);
            }
            if let Some(v) = desc.value {
                self.typed_set(o, i, v)?;
            }
            return Ok(true);
        }
        let (array, string) = match &o.borrow().kind {
            ObjectKind::Array => (true, None),
            ObjectKind::Primitive(v) if let ValueKind::String(s) = v.kind() => (false, Some(s)),
            _ => (false, None),
        };
        match (key, array, string) {
            (Key::Str("length"), true, _) => return self.array_set_length(o, desc),
            (Key::Str(k), true, _) if index_of_str(k).is_some() => {
                let d = o.borrow();
                let i = index_of_str(k).unwrap_or(0);
                if d.length_readonly() && i >= d.array_length().unwrap_or(0) {
                    return Ok(false);
                }
            }
            (Key::Str(k), _, Some(s)) => {
                if let Some(cur) = self.string_own(s, k) {
                    return Ok(validate(false, &desc, Some(cur)).is_some());
                }
            }
            _ => {}
        }
        if let Some(slot) = Self::mapped_slot(o, key) {
            return Ok(self.arguments_define(o, key, desc, slot));
        }
        Ok(self.ordinary_define(o, key, &desc))
    }

    fn mapped_slot(o: JsObject<'scope>, key: Key<'_, 'scope>) -> Option<u16> {
        let Key::Str(k) = key else { return None };
        match &o.borrow().kind {
            ObjectKind::Arguments(Some(map)) => index_of_str(k).and_then(|i| map.slots.get(i as usize).copied().flatten()),
            _ => None,
        }
    }

    fn unmap(&self, o: JsObject<'scope>, key: Key<'_, 'scope>) {
        if let (Key::Str(k), ObjectKind::Arguments(Some(map))) = (key, &mut o.borrow_mut(self.mc).kind)
            && let Some(s) = index_of_str(k).and_then(|i| map.slots.get_mut(i as usize))
        {
            *s = None;
        }
    }

    fn arguments_define(&mut self, o: JsObject<'scope>, key: Key<'_, 'scope>, desc: Descriptor<'scope>, slot: u16) -> bool {
        let env = match &o.borrow().kind {
            ObjectKind::Arguments(Some(map)) => map.env,
            _ => return self.ordinary_define(o, key, &desc),
        };
        let mut d = desc;
        if d.is_data() && d.value.is_none() && d.writable == Some(false) {
            d.value = Some(env.load(usize::from(slot)));
        }
        if !self.ordinary_define(o, key, &d) {
            return false;
        }
        if desc.is_accessor() {
            self.unmap(o, key);
        } else {
            if let Some(v) = desc.value {
                crate::Env::set(env, self.mc, usize::from(slot), v);
            }
            if desc.writable == Some(false) {
                self.unmap(o, key);
            }
        }
        true
    }

    fn ordinary_define(&self, o: JsObject<'scope>, key: Key<'_, 'scope>, desc: &Descriptor<'scope>) -> bool {
        let (current, extensible) = {
            let d = o.borrow();
            (d.own_slot(self.mc, key), !d.non_extensible())
        };
        match validate(extensible, desc, current) {
            Some(p) => {
                o.borrow_mut(self.mc).store(key, p);
                true
            }
            None => false,
        }
    }

    fn array_set_length(&mut self, o: JsObject<'scope>, desc: Descriptor<'scope>) -> Result<bool, Error> {
        let length_prop = |a: &mut Self| a.get_own_property(o, Key::Str("length")).map(|p| p.expect("arrays have a length"));
        let Some(v) = desc.value else {
            let cur = length_prop(self)?;
            return Ok(match validate(true, &desc, Some(cur)) {
                Some(p) => {
                    o.borrow_mut(self.mc).set_length_readonly(!p.writable);
                    true
                }
                None => false,
            });
        };
        let new_len = self.to_uint32(v)?;
        if f64::from(new_len) != self.to_number(v)? {
            return Err(self.throw(ErrorKind::Range, "Invalid array length"));
        }
        let cur = length_prop(self)?;
        let old_len = o.borrow().array_length().unwrap_or(0);
        let new_writable = desc.writable != Some(false);
        let mut nd = Descriptor { value: Some(f64::from(new_len).into()), ..desc };
        if new_len < old_len {
            if !cur.writable {
                return Ok(false);
            }
            nd.writable = Some(true);
        }
        let Some(p) = validate(true, &nd, Some(cur)) else { return Ok(false) };
        let d = o.borrow_mut(self.mc);
        if new_len >= old_len {
            d.set_array_length(new_len);
            d.set_length_readonly(!p.writable);
            return Ok(true);
        }
        let blocker = d.props().entries().into_iter().filter(|(_, p)| !p.configurable).filter_map(|(k, _)| index_of_str(&k)).filter(|&i| i >= new_len).max();
        let ok = blocker.is_none();
        d.set_array_length(blocker.map_or(new_len, |i| i + 1));
        d.set_length_readonly(!new_writable);
        Ok(ok)
    }

    pub fn define_property_or_throw(&mut self, o: JsObject<'scope>, key: Key<'_, 'scope>, desc: Descriptor<'scope>) -> Result<(), Error> {
        if self.define_own_property(o, key, desc)? {
            Ok(())
        } else {
            let k = self.key_text(key);
            Err(self.throw(ErrorKind::Type, &format!("Cannot redefine property: {k}")))
        }
    }

    pub fn create_data_property(&mut self, o: JsObject<'scope>, key: Key<'_, 'scope>, v: Value<'scope>) -> Result<bool, Error> {
        self.define_own_property(o, key, Property::data(v).into())
    }

    pub fn create_data_property_or_throw(&mut self, o: JsObject<'scope>, key: Key<'_, 'scope>, v: Value<'scope>) -> Result<(), Error> {
        if self.create_data_property(o, key, v)? {
            Ok(())
        } else {
            let k = self.key_text(key);
            Err(self.throw(ErrorKind::Type, &format!("Cannot add property {k}, object is not extensible")))
        }
    }

    pub fn key_text(&self, key: Key<'_, 'scope>) -> alloc::string::String {
        match key {
            Key::Str(s) => s.into(),
            Key::Sym(s) => alloc::string::String::from(&**self.symbol_descriptive(s)),
        }
    }

    pub fn delete_property(&mut self, o: JsObject<'scope>, key: Key<'_, 'scope>) -> Result<bool, Error> {
        if let Some(parts) = self.proxy_parts(o) {
            let (t, h) = parts?;
            return self.proxy_delete(t, h, key);
        }
        if let Some(b) = Self::namespace_export(o, key) {
            return Ok(b.is_none());
        }
        if let Some(i) = Self::typed_numeric(o, key) {
            return Ok(!Self::typed_has(o, i));
        }
        match self.get_own_property(o, key)? {
            None => Ok(true),
            Some(p) if p.configurable => {
                o.borrow_mut(self.mc).unstore(key);
                self.unmap(o, key);
                Ok(true)
            }
            Some(_) => Ok(false),
        }
    }

    pub fn own_property_keys(&mut self, o: JsObject<'scope>) -> Result<Vec<PropertyKey<'scope>>, Error> {
        if let Some(parts) = self.proxy_parts(o) {
            let (t, h) = parts?;
            return self.proxy_own_keys(t, h);
        }
        if let Some(keys) = Self::namespace_keys(o) {
            return Ok(keys);
        }
        let d = o.borrow();
        let mut indices: Vec<(u32, Option<JsString<'scope>>)> = Vec::new();
        let mut strings = Vec::new();
        match &d.kind {
            ObjectKind::Array => {
                indices.extend((0..u32::try_from(d.elements().len()).unwrap_or(u32::MAX)).map(|i| (i, None)));
                strings.push(PropertyKey::String(self.string("length")));
            }
            ObjectKind::Primitive(v) if let ValueKind::String(s) = v.kind() => {
                indices.extend((0..u32::try_from(s.utf16_len()).unwrap_or(u32::MAX)).map(|i| (i, None)));
                strings.push(PropertyKey::String(self.string("length")));
            }
            ObjectKind::TypedArray(k, v) => {
                let len = Self::view_length(v, k.size()).unwrap_or(0);
                indices.extend((0..u32::try_from(len).unwrap_or(u32::MAX)).map(|i| (i, None)));
            }
            _ => {}
        }
        for (k, _) in d.props().entries() {
            match index_of_str(&k) {
                Some(i) => indices.push((i, None)),
                None => strings.push(PropertyKey::String(self.string(&k))),
            }
        }
        indices.sort_unstable_by_key(|&(i, _)| i);
        let mut keys: Vec<PropertyKey<'scope>> = indices.into_iter().map(|(i, k)| PropertyKey::String(k.unwrap_or_else(|| self.string(&format!("{i}"))))).collect();
        keys.extend(strings);
        keys.extend(d.symbols.iter().map(|&(s, _)| PropertyKey::Symbol(s)));
        Ok(keys)
    }

    pub fn is_extensible(&mut self, o: JsObject<'scope>) -> Result<bool, Error> {
        if let Some(parts) = self.proxy_parts(o) {
            let (t, h) = parts?;
            return self.proxy_is_extensible(t, h);
        }
        Ok(!o.borrow().non_extensible())
    }

    pub fn prevent_extensions(&mut self, o: JsObject<'scope>) -> Result<bool, Error> {
        if let Some(parts) = self.proxy_parts(o) {
            let (t, h) = parts?;
            return self.proxy_prevent_extensions(t, h);
        }
        o.borrow_mut(self.mc).set_non_extensible(true);
        Ok(true)
    }

    pub fn get_prototype_of(&mut self, o: JsObject<'scope>) -> Result<Option<JsObject<'scope>>, Error> {
        if let Some(parts) = self.proxy_parts(o) {
            let (t, h) = parts?;
            return self.proxy_get_prototype_of(t, h);
        }
        Ok(o.borrow().proto())
    }

    pub fn set_prototype_of(&mut self, o: JsObject<'scope>, proto: Option<JsObject<'scope>>) -> Result<bool, Error> {
        if let Some(parts) = self.proxy_parts(o) {
            let (t, h) = parts?;
            return self.proxy_set_prototype_of(t, h, proto);
        }
        if matches!(o.borrow().kind, ObjectKind::ModuleNamespace(_)) {
            return Ok(proto.is_none());
        }
        let (current, extensible) = {
            let d = o.borrow();
            (d.proto(), !d.non_extensible())
        };
        if same_object(current, proto) {
            return Ok(true);
        }
        if !extensible || ScopeLocal::ptr_eq(o, self.realm.object_proto) {
            return Ok(false);
        }
        let mut p = proto;
        while let Some(x) = p {
            if ScopeLocal::ptr_eq(x, o) {
                return Ok(false);
            }
            if matches!(x.borrow().kind, ObjectKind::Proxy(_)) {
                break;
            }
            p = x.borrow().proto();
        }
        o.borrow_mut(self.mc).set_proto(proto);
        Ok(true)
    }

    pub fn set_integrity_level(&mut self, o: JsObject<'scope>, level: Integrity) -> Result<bool, Error> {
        if !self.prevent_extensions(o)? {
            return Ok(false);
        }
        for k in self.own_property_keys(o)? {
            let key = k.key();
            let desc = match (level, self.get_own_property(o, key)?) {
                (Integrity::Frozen, Some(Property { slot: Slot::Data(_), .. })) => Descriptor { configurable: Some(false), writable: Some(false), ..Descriptor::default() },
                (Integrity::Sealed, _) | (Integrity::Frozen, Some(_)) => Descriptor { configurable: Some(false), ..Descriptor::default() },
                (Integrity::Frozen, None) => continue,
            };
            self.define_property_or_throw(o, key, desc)?;
        }
        Ok(true)
    }

    pub fn test_integrity_level(&mut self, o: JsObject<'scope>, level: Integrity) -> Result<bool, Error> {
        if self.is_extensible(o)? {
            return Ok(false);
        }
        for k in self.own_property_keys(o)? {
            if let Some(p) = self.get_own_property(o, k.key())?
                && (p.configurable || (level == Integrity::Frozen && p.writable && matches!(p.slot, Slot::Data(_))))
            {
                return Ok(false);
            }
        }
        Ok(true)
    }

    pub fn to_property_descriptor(&mut self, v: Value<'scope>) -> Result<Descriptor<'scope>, Error> {
        if v.as_object().is_none() {
            let t = self.describe(v);
            return Err(self.throw(ErrorKind::Type, &format!("Property description must be an object: {t}")));
        }
        let mut d = Descriptor::default();
        if self.has_property(v, "enumerable")? {
            let x = self.get_named(v, "enumerable")?;
            d.enumerable = Some(self.to_boolean(x));
        }
        if self.has_property(v, "configurable")? {
            let x = self.get_named(v, "configurable")?;
            d.configurable = Some(self.to_boolean(x));
        }
        if self.has_property(v, "value")? {
            d.value = Some(self.get_named(v, "value")?);
        }
        if self.has_property(v, "writable")? {
            let x = self.get_named(v, "writable")?;
            d.writable = Some(self.to_boolean(x));
        }
        for (name, field) in [("get", &mut d.get), ("set", &mut d.set)] {
            if self.has_property(v, name)? {
                *field = Some(match (self.get_named(v, name)?).kind() {
                    ValueKind::Undefined => None,
                    ValueKind::Object(f) if f.borrow().kind.is_callable() => Some(f),
                    other => {
                        let t = self.describe(other.into());
                        return Err(self.throw(ErrorKind::Type, &format!("{} must be a function: {t}", if name == "get" { "Getter" } else { "Setter" })));
                    }
                });
            }
        }
        if d.is_accessor() && d.is_data() {
            return Err(self.throw(ErrorKind::Type, "Invalid property descriptor. Cannot both specify accessors and a value or writable attribute"));
        }
        Ok(d)
    }

    pub fn from_property_descriptor(&mut self, p: Option<Property<'scope>>) -> Value<'scope> {
        let Some(p) = p else { return Value::Undefined };
        let o = self.object();
        let d = o.borrow_mut(self.mc);
        match p.slot {
            Slot::Data(v) => {
                d.set("value", v);
                d.set("writable", p.writable.into());
            }
            Slot::Accessor { get, set } => {
                d.set("get", get.map_or(Value::Undefined, Value::Object));
                d.set("set", set.map_or(Value::Undefined, Value::Object));
            }
        }
        d.set("enumerable", p.enumerable.into());
        d.set("configurable", p.configurable.into());
        drop(d);
        o.into()
    }

    pub fn to_property_key(&mut self, v: Value<'scope>) -> Result<PropertyKey<'scope>, Error> {
        match (self.to_primitive(v, crate::Hint::String)?).kind() {
            ValueKind::Symbol(s) => Ok(PropertyKey::Symbol(s)),
            p => Ok(PropertyKey::String(self.to_string(p.into())?)),
        }
    }
}
