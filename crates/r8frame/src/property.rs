//! 对象的内部方法（规范 10.1 普通对象、10.4 数组与字符串包装对象）：自有属性的查 / 定义 / 删、
//! 自有键、可扩展性、原型。属性存取（ops.rs）沿原型链经它们走。
use alloc::format;
use alloc::vec::Vec;

use gc_arena::Gc;

use crate::ops::index_of_str;
use crate::{Agent, Error, ErrorKind, JsObject, JsString, JsSymbol, ObjectKind, Property, Slot, Value};

/// 借来的属性键：字符串键不必先上堆。
#[derive(Clone, Copy, Debug)]
pub enum Key<'k, 'gc> {
    Str(&'k str),
    Sym(JsSymbol<'gc>),
}

impl<'k> From<&'k str> for Key<'k, '_> {
    fn from(s: &'k str) -> Self {
        Key::Str(s)
    }
}

/// 堆上的属性键（自有键列表里的一项）。
#[derive(Clone, Copy, Debug)]
pub enum PropertyKey<'gc> {
    String(JsString<'gc>),
    Symbol(JsSymbol<'gc>),
}

impl<'gc> PropertyKey<'gc> {
    pub fn key(&self) -> Key<'_, 'gc> {
        match self {
            PropertyKey::String(s) => Key::Str(s),
            PropertyKey::Symbol(s) => Key::Sym(*s),
        }
    }
}

impl<'gc> From<PropertyKey<'gc>> for Value<'gc> {
    fn from(k: PropertyKey<'gc>) -> Self {
        match k {
            PropertyKey::String(s) => Value::String(s),
            PropertyKey::Symbol(s) => Value::Symbol(s),
        }
    }
}

/// 属性描述符：每个字段都可能缺（`Object.defineProperty` 只改给了的）。
/// `get` / `set` 外层 `None` 是没给，`Some(None)` 是给了 `undefined`。
#[derive(Clone, Copy, Debug, Default)]
pub struct Descriptor<'gc> {
    pub value: Option<Value<'gc>>,
    pub writable: Option<bool>,
    pub get: Option<Option<JsObject<'gc>>>,
    pub set: Option<Option<JsObject<'gc>>>,
    pub enumerable: Option<bool>,
    pub configurable: Option<bool>,
}

impl<'gc> Descriptor<'gc> {
    pub fn is_accessor(&self) -> bool {
        self.get.is_some() || self.set.is_some()
    }

    pub fn is_data(&self) -> bool {
        self.value.is_some() || self.writable.is_some()
    }

    pub fn is_generic(&self) -> bool {
        !self.is_accessor() && !self.is_data()
    }

    /// 只给值（赋值改已有属性用）。
    pub fn value(v: Value<'gc>) -> Self {
        Self { value: Some(v), ..Self::default() }
    }

    /// 缺的字段补缺省值（`false` / `undefined`）。
    pub fn complete(&self) -> Property<'gc> {
        let slot = if self.is_accessor() { Slot::Accessor { get: self.get.flatten(), set: self.set.flatten() } } else { Slot::Data(self.value.unwrap_or_default()) };
        Property { slot, enumerable: self.enumerable.unwrap_or(false), writable: self.writable.unwrap_or(false), configurable: self.configurable.unwrap_or(false) }
    }
}

/// 完整的属性 ⇒ 每个字段都给了的描述符。
impl<'gc> From<Property<'gc>> for Descriptor<'gc> {
    fn from(p: Property<'gc>) -> Self {
        let base = Self { enumerable: Some(p.enumerable), configurable: Some(p.configurable), ..Self::default() };
        match p.slot {
            Slot::Data(v) => Self { value: Some(v), writable: Some(p.writable), ..base },
            Slot::Accessor { get, set } => Self { get: Some(get), set: Some(set), ..base },
        }
    }
}

/// SetIntegrityLevel / TestIntegrityLevel 的档。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Integrity {
    Sealed,
    Frozen,
}

fn same_object<'gc>(a: Option<JsObject<'gc>>, b: Option<JsObject<'gc>>) -> bool {
    match (a, b) {
        (Some(x), Some(y)) => Gc::ptr_eq(x, y),
        (None, None) => true,
        _ => false,
    }
}

/// ValidateAndApplyPropertyDescriptor：能改就给出改完的属性，不能给 `None`。
pub fn validate<'gc>(extensible: bool, desc: &Descriptor<'gc>, current: Option<Property<'gc>>) -> Option<Property<'gc>> {
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

impl<'gc> Agent<'gc, '_> {
    /// 字符串包装对象的下标与 `length`（不可写、不可配置）。
    fn string_own(&self, s: JsString<'gc>, key: &str) -> Option<Property<'gc>> {
        let fixed = |v: Value<'gc>, enumerable| Property { slot: Slot::Data(v), enumerable, writable: false, configurable: false };
        if key == "length" {
            return Some(fixed(f64::from(u32::try_from(s.encode_utf16().count()).unwrap_or(u32::MAX)).into(), false));
        }
        let unit = s.encode_utf16().nth(index_of_str(key)? as usize)?;
        Some(fixed(self.string(&alloc::string::String::from_utf16_lossy(&[unit])).into(), true))
    }

    /// [[GetOwnProperty]]。
    pub fn get_own_property(&mut self, o: JsObject<'gc>, key: Key<'_, 'gc>) -> Result<Option<Property<'gc>>, Error> {
        let d = o.borrow();
        Ok(match (&d.kind, key) {
            (ObjectKind::Array(_), Key::Str("length")) => {
                let len = f64::from(d.array_length().unwrap_or(0));
                Some(Property { slot: Slot::Data(len.into()), enumerable: false, writable: !d.length_readonly, configurable: false })
            }
            (ObjectKind::Primitive(Value::String(s)), Key::Str(k)) => self.string_own(*s, k).or_else(|| d.own_slot(key)),
            _ => d.own_slot(key),
        })
    }

    pub fn has_own_property(&mut self, o: JsObject<'gc>, key: Key<'_, 'gc>) -> Result<bool, Error> {
        Ok(self.get_own_property(o, key)?.is_some())
    }

    /// [[DefineOwnProperty]]：给出成没成。
    pub fn define_own_property(&mut self, o: JsObject<'gc>, key: Key<'_, 'gc>, desc: Descriptor<'gc>) -> Result<bool, Error> {
        let (array, string) = match &o.borrow().kind {
            ObjectKind::Array(_) => (true, None),
            ObjectKind::Primitive(Value::String(s)) => (false, Some(*s)),
            _ => (false, None),
        };
        match (key, array, string) {
            (Key::Str("length"), true, _) => return self.array_set_length(o, desc),
            (Key::Str(k), true, _) if index_of_str(k).is_some() => {
                let d = o.borrow();
                let i = index_of_str(k).unwrap_or(0);
                if d.length_readonly && i >= d.array_length().unwrap_or(0) {
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
        Ok(self.ordinary_define(o, key, &desc))
    }

    /// OrdinaryDefineOwnProperty。
    fn ordinary_define(&self, o: JsObject<'gc>, key: Key<'_, 'gc>, desc: &Descriptor<'gc>) -> bool {
        let (current, extensible) = {
            let d = o.borrow();
            (d.own_slot(key), !d.non_extensible)
        };
        match validate(extensible, desc, current) {
            Some(p) => {
                o.borrow_mut(self.mc).store(self.mc, key, p);
                true
            }
            None => false,
        }
    }

    /// ArraySetLength。
    fn array_set_length(&mut self, o: JsObject<'gc>, desc: Descriptor<'gc>) -> Result<bool, Error> {
        let length_prop = |a: &mut Self| a.get_own_property(o, Key::Str("length")).map(|p| p.expect("arrays have a length"));
        let Some(v) = desc.value else {
            let cur = length_prop(self)?;
            return Ok(match validate(true, &desc, Some(cur)) {
                Some(p) => {
                    o.borrow_mut(self.mc).length_readonly = !p.writable;
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
        let mut d = o.borrow_mut(self.mc);
        if new_len >= old_len {
            d.set_array_length(new_len);
            d.length_readonly = !p.writable;
            return Ok(true);
        }
        // 从后往前删；碰到不可配置的元素就停在它后面
        let blocker = d.props.iter().filter(|(_, p)| !p.configurable).filter_map(|(k, _)| index_of_str(k)).filter(|&i| i >= new_len).max();
        let ok = blocker.is_none();
        d.set_array_length(blocker.map_or(new_len, |i| i + 1));
        d.length_readonly = !new_writable;
        Ok(ok)
    }

    /// DefinePropertyOrThrow。
    pub fn define_property_or_throw(&mut self, o: JsObject<'gc>, key: Key<'_, 'gc>, desc: Descriptor<'gc>) -> Result<(), Error> {
        if self.define_own_property(o, key, desc)? {
            Ok(())
        } else {
            let k = self.key_text(key);
            Err(self.throw(ErrorKind::Type, &format!("Cannot redefine property: {k}")))
        }
    }

    /// CreateDataProperty：可写可枚举可配置的数据属性。
    pub fn create_data_property(&mut self, o: JsObject<'gc>, key: Key<'_, 'gc>, v: Value<'gc>) -> Result<bool, Error> {
        self.define_own_property(o, key, Property::data(v).into())
    }

    /// CreateDataPropertyOrThrow。
    pub fn create_data_property_or_throw(&mut self, o: JsObject<'gc>, key: Key<'_, 'gc>, v: Value<'gc>) -> Result<(), Error> {
        if self.create_data_property(o, key, v)? {
            Ok(())
        } else {
            let k = self.key_text(key);
            Err(self.throw(ErrorKind::Type, &format!("Cannot add property {k}, object is not extensible")))
        }
    }

    /// 报错里的键。
    pub fn key_text(&self, key: Key<'_, 'gc>) -> alloc::string::String {
        match key {
            Key::Str(s) => s.into(),
            Key::Sym(s) => alloc::string::String::from(&**self.symbol_descriptive(s)),
        }
    }

    /// [[Delete]]：不可配置的删不掉（给 `false`），没有也算删成。
    pub fn delete_property(&mut self, o: JsObject<'gc>, key: Key<'_, 'gc>) -> Result<bool, Error> {
        match self.get_own_property(o, key)? {
            None => Ok(true),
            Some(p) if p.configurable => {
                o.borrow_mut(self.mc).unstore(self.mc, key);
                Ok(true)
            }
            Some(_) => Ok(false),
        }
    }

    /// [[OwnPropertyKeys]]：数组下标从小到大，再字符串键按建立顺序，再 Symbol 键按建立顺序。
    pub fn own_property_keys(&mut self, o: JsObject<'gc>) -> Result<Vec<PropertyKey<'gc>>, Error> {
        let d = o.borrow();
        let mut indices: Vec<(u32, Option<JsString<'gc>>)> = Vec::new();
        let mut strings = Vec::new();
        match &d.kind {
            ObjectKind::Array(items) => {
                indices.extend((0..u32::try_from(items.len()).unwrap_or(u32::MAX)).map(|i| (i, None)));
                strings.push(PropertyKey::String(self.string("length")));
            }
            ObjectKind::Primitive(Value::String(s)) => {
                indices.extend((0..u32::try_from(s.encode_utf16().count()).unwrap_or(u32::MAX)).map(|i| (i, None)));
                strings.push(PropertyKey::String(self.string("length")));
            }
            _ => {}
        }
        for &(k, _) in &d.props {
            match index_of_str(&k) {
                Some(i) => indices.push((i, Some(k))),
                None => strings.push(PropertyKey::String(k)),
            }
        }
        indices.sort_unstable_by_key(|&(i, _)| i);
        let mut keys: Vec<PropertyKey<'gc>> = indices.into_iter().map(|(i, k)| PropertyKey::String(k.unwrap_or_else(|| self.string(&format!("{i}"))))).collect();
        keys.extend(strings);
        keys.extend(d.symbols.iter().map(|&(s, _)| PropertyKey::Symbol(s)));
        Ok(keys)
    }

    pub fn is_extensible(&mut self, o: JsObject<'gc>) -> Result<bool, Error> {
        Ok(!o.borrow().non_extensible)
    }

    pub fn prevent_extensions(&mut self, o: JsObject<'gc>) -> Result<bool, Error> {
        o.borrow_mut(self.mc).non_extensible = true;
        Ok(true)
    }

    pub fn get_prototype_of(&mut self, o: JsObject<'gc>) -> Result<Option<JsObject<'gc>>, Error> {
        Ok(o.borrow().proto)
    }

    /// [[SetPrototypeOf]]：不可扩展的只能设成原来那个；成环不行；`Object.prototype` 的原型不可改。
    pub fn set_prototype_of(&mut self, o: JsObject<'gc>, proto: Option<JsObject<'gc>>) -> Result<bool, Error> {
        let (current, extensible) = {
            let d = o.borrow();
            (d.proto, !d.non_extensible)
        };
        if same_object(current, proto) {
            return Ok(true);
        }
        if !extensible || Gc::ptr_eq(o, self.realm.object_proto) {
            return Ok(false);
        }
        let mut p = proto;
        while let Some(x) = p {
            if Gc::ptr_eq(x, o) {
                return Ok(false);
            }
            p = x.borrow().proto;
        }
        o.borrow_mut(self.mc).proto = proto;
        Ok(true)
    }

    /// SetIntegrityLevel。
    pub fn set_integrity_level(&mut self, o: JsObject<'gc>, level: Integrity) -> Result<bool, Error> {
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

    /// TestIntegrityLevel。
    pub fn test_integrity_level(&mut self, o: JsObject<'gc>, level: Integrity) -> Result<bool, Error> {
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

    /// ToPropertyDescriptor。
    pub fn to_property_descriptor(&mut self, v: Value<'gc>) -> Result<Descriptor<'gc>, Error> {
        if !matches!(v, Value::Object(_)) {
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
                *field = Some(match self.get_named(v, name)? {
                    Value::Undefined => None,
                    Value::Object(f) if f.borrow().kind.is_callable() => Some(f),
                    other => {
                        let t = self.describe(other);
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

    /// FromPropertyDescriptor：没有属性给 `undefined`。
    pub fn from_property_descriptor(&mut self, p: Option<Property<'gc>>) -> Value<'gc> {
        let Some(p) = p else { return Value::Undefined };
        let o = self.object();
        let mut d = o.borrow_mut(self.mc);
        match p.slot {
            Slot::Data(v) => {
                d.set(self.mc, "value", v);
                d.set(self.mc, "writable", p.writable.into());
            }
            Slot::Accessor { get, set } => {
                d.set(self.mc, "get", get.map_or(Value::Undefined, Value::Object));
                d.set(self.mc, "set", set.map_or(Value::Undefined, Value::Object));
            }
        }
        d.set(self.mc, "enumerable", p.enumerable.into());
        d.set(self.mc, "configurable", p.configurable.into());
        drop(d);
        o.into()
    }

    /// 把一个值当属性键（ToPropertyKey）：Symbol 原样，别的 ToString。
    pub fn to_property_key(&mut self, v: Value<'gc>) -> Result<PropertyKey<'gc>, Error> {
        match self.to_primitive(v, crate::Hint::String)? {
            Value::Symbol(s) => Ok(PropertyKey::Symbol(s)),
            p => Ok(PropertyKey::String(self.to_string(p)?)),
        }
    }
}
