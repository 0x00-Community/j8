//! Proxy 的内部方法（规范 10.5）：每个入口先找处理器上的 trap，没有就转给目标，有就调它再查不变式。
use alloc::vec::Vec;

use crate::property::validate;
use crate::{Agent, Descriptor, Error, ErrorKind, JsObject, Key, ObjectKind, Property, PropertyKey, Slot, Value};

/// 处理器上的 trap 名。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Trap {
    GetPrototypeOf,
    SetPrototypeOf,
    IsExtensible,
    PreventExtensions,
    GetOwnPropertyDescriptor,
    DefineProperty,
    Has,
    Get,
    Set,
    DeleteProperty,
    OwnKeys,
    Apply,
    Construct,
}

impl Trap {
    pub fn name(self) -> &'static str {
        match self {
            Trap::GetPrototypeOf => "getPrototypeOf",
            Trap::SetPrototypeOf => "setPrototypeOf",
            Trap::IsExtensible => "isExtensible",
            Trap::PreventExtensions => "preventExtensions",
            Trap::GetOwnPropertyDescriptor => "getOwnPropertyDescriptor",
            Trap::DefineProperty => "defineProperty",
            Trap::Has => "has",
            Trap::Get => "get",
            Trap::Set => "set",
            Trap::DeleteProperty => "deleteProperty",
            Trap::OwnKeys => "ownKeys",
            Trap::Apply => "apply",
            Trap::Construct => "construct",
        }
    }
}

impl<'gc> Agent<'gc, '_> {
    /// 是 Proxy 就给出（目标, 处理器）；撤销了抛 TypeError。
    pub fn proxy_parts(&self, o: JsObject<'gc>) -> Option<Result<(JsObject<'gc>, JsObject<'gc>), Error>> {
        match o.borrow().kind {
            ObjectKind::Proxy(p) => Some(match (p.target, p.handler) {
                (Some(t), Some(h)) => Ok((t, h)),
                _ => Err(self.throw(ErrorKind::Type, "Cannot perform operation on a revoked proxy")),
            }),
            _ => None,
        }
    }

    /// 处理器上的 trap：`undefined` / `null` 给 `None`，不可调用抛 TypeError。
    fn trap(&mut self, handler: JsObject<'gc>, trap: Trap) -> Result<Option<Value<'gc>>, Error> {
        match self.get_named(handler.into(), trap.name())? {
            Value::Undefined | Value::Null => Ok(None),
            f if self.is_callable(f) => Ok(Some(f)),
            _ => Err(self.throw(ErrorKind::Type, &alloc::format!("proxy trap '{}' is not a function", trap.name()))),
        }
    }

    /// 键 ⇒ 值（交给 trap 的实参）。
    pub fn key_value(&self, key: Key<'_, 'gc>) -> Value<'gc> {
        match key {
            Key::Str(s) => self.string(s).into(),
            Key::Sym(s) => s.into(),
        }
    }

    fn invariant(&self, what: &str) -> Error {
        self.throw(ErrorKind::Type, &alloc::format!("proxy invariant violated: {what}"))
    }

    pub(crate) fn proxy_get_prototype_of(&mut self, target: JsObject<'gc>, handler: JsObject<'gc>) -> Result<Option<JsObject<'gc>>, Error> {
        let Some(trap) = self.trap(handler, Trap::GetPrototypeOf)? else { return self.get_prototype_of(target) };
        let proto = match self.call(trap, handler.into(), &[target.into()])? {
            Value::Object(p) => Some(p),
            Value::Null => None,
            _ => return Err(self.throw(ErrorKind::Type, "getPrototypeOf trap returned neither an object nor null")),
        };
        if self.is_extensible(target)? {
            return Ok(proto);
        }
        let target_proto = self.get_prototype_of(target)?;
        if Value::from_proto(proto) != Value::from_proto(target_proto) {
            return Err(self.invariant("getPrototypeOf of a non-extensible target must report its prototype"));
        }
        Ok(proto)
    }

    pub(crate) fn proxy_set_prototype_of(&mut self, target: JsObject<'gc>, handler: JsObject<'gc>, proto: Option<JsObject<'gc>>) -> Result<bool, Error> {
        let Some(trap) = self.trap(handler, Trap::SetPrototypeOf)? else { return self.set_prototype_of(target, proto) };
        let r = self.call(trap, handler.into(), &[target.into(), Value::from_proto(proto)])?;
        if !self.to_boolean(r) {
            return Ok(false);
        }
        if self.is_extensible(target)? {
            return Ok(true);
        }
        let target_proto = self.get_prototype_of(target)?;
        if Value::from_proto(proto) != Value::from_proto(target_proto) {
            return Err(self.invariant("setPrototypeOf on a non-extensible target must not change its prototype"));
        }
        Ok(true)
    }

    pub(crate) fn proxy_is_extensible(&mut self, target: JsObject<'gc>, handler: JsObject<'gc>) -> Result<bool, Error> {
        let Some(trap) = self.trap(handler, Trap::IsExtensible)? else { return self.is_extensible(target) };
        let r = self.call(trap, handler.into(), &[target.into()])?;
        let r = self.to_boolean(r);
        if r != self.is_extensible(target)? {
            return Err(self.invariant("isExtensible must report the target's extensibility"));
        }
        Ok(r)
    }

    pub(crate) fn proxy_prevent_extensions(&mut self, target: JsObject<'gc>, handler: JsObject<'gc>) -> Result<bool, Error> {
        let Some(trap) = self.trap(handler, Trap::PreventExtensions)? else { return self.prevent_extensions(target) };
        let r = self.call(trap, handler.into(), &[target.into()])?;
        let r = self.to_boolean(r);
        if r && self.is_extensible(target)? {
            return Err(self.invariant("preventExtensions returned true but the target is still extensible"));
        }
        Ok(r)
    }

    pub(crate) fn proxy_get_own_property(&mut self, target: JsObject<'gc>, handler: JsObject<'gc>, key: Key<'_, 'gc>) -> Result<Option<Property<'gc>>, Error> {
        let Some(trap) = self.trap(handler, Trap::GetOwnPropertyDescriptor)? else { return self.get_own_property(target, key) };
        let k = self.key_value(key);
        let result = self.call(trap, handler.into(), &[target.into(), k])?;
        if !matches!(result, Value::Object(_) | Value::Undefined) {
            return Err(self.throw(ErrorKind::Type, "getOwnPropertyDescriptor trap returned neither an object nor undefined"));
        }
        let target_desc = self.get_own_property(target, key)?;
        if result == Value::Undefined {
            let Some(td) = target_desc else { return Ok(None) };
            if !td.configurable {
                return Err(self.invariant("cannot report a non-configurable property as non-existent"));
            }
            if !self.is_extensible(target)? {
                return Err(self.invariant("cannot report an existing property of a non-extensible target as non-existent"));
            }
            return Ok(None);
        }
        let extensible = self.is_extensible(target)?;
        let desc = self.to_property_descriptor(result)?;
        let full = desc.complete();
        if validate(extensible, &full.into(), target_desc).is_none() {
            return Err(self.invariant("getOwnPropertyDescriptor reported an incompatible descriptor"));
        }
        if !full.configurable {
            match target_desc {
                None | Some(Property { configurable: true, .. }) => return Err(self.invariant("cannot report a configurable or missing property as non-configurable")),
                Some(td) if desc.writable == Some(false) && td.writable && matches!(td.slot, Slot::Data(_)) => {
                    return Err(self.invariant("cannot report a writable property as non-configurable and non-writable"));
                }
                _ => {}
            }
        }
        Ok(Some(full))
    }

    /// 部分描述符 ⇒ 对象（只放给了的字段）。
    pub fn from_partial_descriptor(&mut self, d: &Descriptor<'gc>) -> Value<'gc> {
        let o = self.object();
        let mut data = o.borrow_mut(self.mc);
        if let Some(v) = d.value {
            data.set(self.mc, "value", v);
        }
        if let Some(w) = d.writable {
            data.set(self.mc, "writable", w.into());
        }
        if let Some(g) = d.get {
            data.set(self.mc, "get", g.map_or(Value::Undefined, Value::Object));
        }
        if let Some(s) = d.set {
            data.set(self.mc, "set", s.map_or(Value::Undefined, Value::Object));
        }
        if let Some(e) = d.enumerable {
            data.set(self.mc, "enumerable", e.into());
        }
        if let Some(c) = d.configurable {
            data.set(self.mc, "configurable", c.into());
        }
        drop(data);
        o.into()
    }

    pub(crate) fn proxy_define_own_property(&mut self, target: JsObject<'gc>, handler: JsObject<'gc>, key: Key<'_, 'gc>, desc: Descriptor<'gc>) -> Result<bool, Error> {
        let Some(trap) = self.trap(handler, Trap::DefineProperty)? else { return self.define_own_property(target, key, desc) };
        let k = self.key_value(key);
        let d = self.from_partial_descriptor(&desc);
        let r = self.call(trap, handler.into(), &[target.into(), k, d])?;
        if !self.to_boolean(r) {
            return Ok(false);
        }
        let target_desc = self.get_own_property(target, key)?;
        let extensible = self.is_extensible(target)?;
        let setting_config_false = desc.configurable == Some(false);
        match target_desc {
            None => {
                if !extensible {
                    return Err(self.invariant("cannot add a property to a non-extensible target"));
                }
                if setting_config_false {
                    return Err(self.invariant("cannot define a non-configurable property that does not exist on the target"));
                }
            }
            Some(td) => {
                if validate(extensible, &desc, Some(td)).is_none() {
                    return Err(self.invariant("defineProperty is incompatible with the target's property"));
                }
                if setting_config_false && td.configurable {
                    return Err(self.invariant("cannot define a configurable target property as non-configurable"));
                }
                if matches!(td.slot, Slot::Data(_)) && !td.configurable && td.writable && desc.writable == Some(false) {
                    return Err(self.invariant("cannot make a non-configurable writable property non-writable"));
                }
            }
        }
        Ok(true)
    }

    pub(crate) fn proxy_has(&mut self, target: JsObject<'gc>, handler: JsObject<'gc>, key: Key<'_, 'gc>) -> Result<bool, Error> {
        let Some(trap) = self.trap(handler, Trap::Has)? else { return self.has_key(target, key) };
        let k = self.key_value(key);
        let r = self.call(trap, handler.into(), &[target.into(), k])?;
        let r = self.to_boolean(r);
        if !r && let Some(td) = self.get_own_property(target, key)? {
            if !td.configurable {
                return Err(self.invariant("cannot report a non-configurable property as non-existent"));
            }
            if !self.is_extensible(target)? {
                return Err(self.invariant("cannot report a property of a non-extensible target as non-existent"));
            }
        }
        Ok(r)
    }

    pub(crate) fn proxy_get(&mut self, target: JsObject<'gc>, handler: JsObject<'gc>, key: Key<'_, 'gc>, receiver: Value<'gc>) -> Result<Value<'gc>, Error> {
        let Some(trap) = self.trap(handler, Trap::Get)? else { return self.object_get(target, key, receiver) };
        let k = self.key_value(key);
        let v = self.call(trap, handler.into(), &[target.into(), k, receiver])?;
        if let Some(td) = self.get_own_property(target, key)?
            && !td.configurable
        {
            match td.slot {
                Slot::Data(x) if !td.writable && !v.same_value(x) => return Err(self.invariant("get must report the value of a non-writable, non-configurable property")),
                Slot::Accessor { get: None, .. } if v != Value::Undefined => return Err(self.invariant("get must report undefined for a non-configurable accessor without a getter")),
                _ => {}
            }
        }
        Ok(v)
    }

    pub(crate) fn proxy_set(&mut self, target: JsObject<'gc>, handler: JsObject<'gc>, key: Key<'_, 'gc>, value: Value<'gc>, receiver: Value<'gc>) -> Result<bool, Error> {
        let Some(trap) = self.trap(handler, Trap::Set)? else { return self.object_set(target, key, value, receiver) };
        let k = self.key_value(key);
        let r = self.call(trap, handler.into(), &[target.into(), k, value, receiver])?;
        if !self.to_boolean(r) {
            return Ok(false);
        }
        if let Some(td) = self.get_own_property(target, key)?
            && !td.configurable
        {
            match td.slot {
                Slot::Data(x) if !td.writable && !value.same_value(x) => return Err(self.invariant("cannot change a non-writable, non-configurable property")),
                Slot::Accessor { set: None, .. } => return Err(self.invariant("cannot set a non-configurable accessor without a setter")),
                _ => {}
            }
        }
        Ok(true)
    }

    pub(crate) fn proxy_delete(&mut self, target: JsObject<'gc>, handler: JsObject<'gc>, key: Key<'_, 'gc>) -> Result<bool, Error> {
        let Some(trap) = self.trap(handler, Trap::DeleteProperty)? else { return self.delete_property(target, key) };
        let k = self.key_value(key);
        let r = self.call(trap, handler.into(), &[target.into(), k])?;
        if !self.to_boolean(r) {
            return Ok(false);
        }
        let Some(td) = self.get_own_property(target, key)? else { return Ok(true) };
        if !td.configurable {
            return Err(self.invariant("cannot delete a non-configurable property"));
        }
        if !self.is_extensible(target)? {
            return Err(self.invariant("cannot delete a property of a non-extensible target"));
        }
        Ok(true)
    }

    pub(crate) fn proxy_own_keys(&mut self, target: JsObject<'gc>, handler: JsObject<'gc>) -> Result<Vec<PropertyKey<'gc>>, Error> {
        let Some(trap) = self.trap(handler, Trap::OwnKeys)? else { return self.own_property_keys(target) };
        let arr = self.call(trap, handler.into(), &[target.into()])?;
        if !matches!(arr, Value::Object(_)) {
            return Err(self.throw(ErrorKind::Type, "ownKeys trap result must be an object"));
        }
        let mut keys: Vec<PropertyKey<'gc>> = Vec::new();
        for v in self.list_from_array_like(arr)? {
            let k = match v {
                Value::String(s) => PropertyKey::String(s),
                Value::Symbol(s) => PropertyKey::Symbol(s),
                _ => return Err(self.throw(ErrorKind::Type, "ownKeys trap result must contain only strings and symbols")),
            };
            if keys.iter().any(|x| Self::same_key(x, &k)) {
                return Err(self.throw(ErrorKind::Type, "ownKeys trap result contains duplicate entries"));
            }
            keys.push(k);
        }
        let extensible = self.is_extensible(target)?;
        let target_keys = self.own_property_keys(target)?;
        let (mut configurable, mut fixed) = (Vec::new(), Vec::new());
        for k in target_keys {
            match self.get_own_property(target, k.key())? {
                Some(p) if !p.configurable => fixed.push(k),
                _ => configurable.push(k),
            }
        }
        if extensible && fixed.is_empty() {
            return Ok(keys);
        }
        let mut unchecked: Vec<PropertyKey<'gc>> = keys.clone();
        for k in &fixed {
            match unchecked.iter().position(|x| Self::same_key(x, k)) {
                Some(i) => _ = unchecked.remove(i),
                None => return Err(self.invariant("ownKeys must report every non-configurable key of the target")),
            }
        }
        if extensible {
            return Ok(keys);
        }
        for k in &configurable {
            match unchecked.iter().position(|x| Self::same_key(x, k)) {
                Some(i) => _ = unchecked.remove(i),
                None => return Err(self.invariant("ownKeys must report every key of a non-extensible target")),
            }
        }
        if !unchecked.is_empty() {
            return Err(self.invariant("ownKeys cannot report extra keys for a non-extensible target"));
        }
        Ok(keys)
    }

    fn same_key(a: &PropertyKey<'gc>, b: &PropertyKey<'gc>) -> bool {
        match (a, b) {
            (PropertyKey::String(x), PropertyKey::String(y)) => ***x == ***y,
            (PropertyKey::Symbol(x), PropertyKey::Symbol(y)) => gc_arena::Gc::ptr_eq(*x, *y),
            _ => false,
        }
    }

    /// Proxy 的 [[Call]]。
    pub fn proxy_call(&mut self, o: JsObject<'gc>, this: Value<'gc>, args: &[Value<'gc>]) -> Result<Value<'gc>, Error> {
        let (target, handler) = self.proxy_parts(o).expect("a proxy")?;
        let Some(trap) = self.trap(handler, Trap::Apply)? else { return self.call(target.into(), this, args) };
        let list = self.array(args.to_vec());
        self.call(trap, handler.into(), &[target.into(), this, list.into()])
    }

    /// Proxy 的 [[Construct]]。
    pub fn proxy_construct(&mut self, o: JsObject<'gc>, args: &[Value<'gc>], new_target: Value<'gc>) -> Result<Value<'gc>, Error> {
        let (target, handler) = self.proxy_parts(o).expect("a proxy")?;
        let Some(trap) = self.trap(handler, Trap::Construct)? else { return self.construct(target.into(), args, new_target) };
        let list = self.array(args.to_vec());
        match self.call(trap, handler.into(), &[target.into(), list.into(), new_target])? {
            v @ Value::Object(_) => Ok(v),
            _ => Err(self.throw(ErrorKind::Type, "proxy [[Construct]] must return an object")),
        }
    }
}
