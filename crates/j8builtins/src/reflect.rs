use j8framework::ValueKind;
use j8framework::{Agent, Error, ErrorKind, JsObject, NativeClosure, ObjectData, ObjectKind, Property, ProxyData, Value};

use crate::{Args, Installer};

/// `Proxy` and `Reflect`.
struct Reflects;

impl Reflects {
    /// ProxyCreate: target and handler must both be objects.
    fn create<'gc>(a: &mut Agent<'gc, '_>, target: Value<'gc>, handler: Value<'gc>) -> Result<JsObject<'gc>, Error> {
        let (ValueKind::Object(t), ValueKind::Object(h)) = (target.kind(), handler.kind()) else {
            return Err(a.throw(ErrorKind::Type, "Cannot create proxy with a non-object as target or handler"));
        };
        let data = ProxyData { target: Some(t), handler: Some(h), callable: a.is_callable(target), constructor: a.is_constructor(target) };
        Ok(ObjectData { kind: ObjectKind::Proxy(data), ..ObjectData::default() }.alloc(a.mc))
    }

    fn proxy<'gc>(a: &mut Agent<'gc, '_>, _: Value<'gc>, args: &[Value<'gc>]) -> Result<Value<'gc>, Error> {
        let new_target = a.new_target;
        if new_target == Value::Undefined {
            return Err(a.throw(ErrorKind::Type, "Constructor Proxy requires 'new'"));
        }
        Ok(Self::create(a, args.at(0), args.at(1))?.into())
    }

    /// `Proxy.revocable(target, handler)`: `{ proxy, revoke }`.
    fn revocable<'gc>(a: &mut Agent<'gc, '_>, _: Value<'gc>, args: &[Value<'gc>]) -> Result<Value<'gc>, Error> {
        let p = Self::create(a, args.at(0), args.at(1))?;
        let revoke = ObjectData { kind: ObjectKind::NativeClosure(NativeClosure { f: Self::revoke, slots: alloc::vec![p.into()], constructor: false }), proto: Some(a.realm.function_proto), ..ObjectData::default() }.alloc(a.mc);
        a.realm.name_function(a.mc, revoke, "", Some(0));
        let out = a.object();
        a.create_data_property_or_throw(out, j8framework::Key::Str("proxy"), p.into())?;
        a.create_data_property_or_throw(out, j8framework::Key::Str("revoke"), revoke.into())?;
        Ok(out.into())
    }

    /// Revocation function: only the first call has an effect.
    fn revoke<'gc>(a: &mut Agent<'gc, '_>, f: JsObject<'gc>, _: Value<'gc>, _: &[Value<'gc>]) -> Result<Value<'gc>, Error> {
        let target = match &mut f.borrow_mut(a.mc).kind {
            ObjectKind::NativeClosure(c) => core::mem::take(&mut c.slots[0]),
            _ => Value::Undefined,
        };
        if let Some(p) = target.as_object()
            && let ObjectKind::Proxy(d) = &mut p.borrow_mut(a.mc).kind
        {
            d.target = None;
            d.handler = None;
        }
        Ok(Value::Undefined)
    }

    fn target<'gc>(a: &Agent<'gc, '_>, v: Value<'gc>, method: &str) -> Result<JsObject<'gc>, Error> {
        match v.kind() {
            ValueKind::Object(o) => Ok(o),
            _ => Err(a.throw(ErrorKind::Type, &alloc::format!("Reflect.{method} called on non-object"))),
        }
    }

    fn apply<'gc>(a: &mut Agent<'gc, '_>, _: Value<'gc>, args: &[Value<'gc>]) -> Result<Value<'gc>, Error> {
        let f = args.at(0);
        if !a.is_callable(f) {
            let t = a.describe(f);
            return Err(a.throw(ErrorKind::Type, &alloc::format!("{t} is not a function")));
        }
        let list = a.list_from_array_like(args.at(2))?;
        a.call(f, args.at(1), &list)
    }

    fn construct<'gc>(a: &mut Agent<'gc, '_>, _: Value<'gc>, args: &[Value<'gc>]) -> Result<Value<'gc>, Error> {
        let f = args.at(0);
        if !a.is_constructor(f) {
            let t = a.describe(f);
            return Err(a.throw(ErrorKind::Type, &alloc::format!("{t} is not a constructor")));
        }
        let new_target = if args.len() > 2 { args[2] } else { f };
        if !a.is_constructor(new_target) {
            let t = a.describe(new_target);
            return Err(a.throw(ErrorKind::Type, &alloc::format!("{t} is not a constructor")));
        }
        let list = a.list_from_array_like(args.at(1))?;
        a.construct(f, &list, new_target)
    }

    fn define_property<'gc>(a: &mut Agent<'gc, '_>, _: Value<'gc>, args: &[Value<'gc>]) -> Result<Value<'gc>, Error> {
        let o = Self::target(a, args.at(0), "defineProperty")?;
        let key = a.to_property_key(args.at(1))?;
        let desc = a.to_property_descriptor(args.at(2))?;
        Ok(a.define_own_property(o, key.key(), desc)?.into())
    }

    fn delete_property<'gc>(a: &mut Agent<'gc, '_>, _: Value<'gc>, args: &[Value<'gc>]) -> Result<Value<'gc>, Error> {
        let o = Self::target(a, args.at(0), "deleteProperty")?;
        let key = a.to_property_key(args.at(1))?;
        Ok(a.delete_property(o, key.key())?.into())
    }

    fn get<'gc>(a: &mut Agent<'gc, '_>, _: Value<'gc>, args: &[Value<'gc>]) -> Result<Value<'gc>, Error> {
        let o = Self::target(a, args.at(0), "get")?;
        let key = a.to_property_key(args.at(1))?;
        let receiver = if args.len() > 2 { args[2] } else { o.into() };
        a.object_get(o, key.key(), receiver)
    }

    fn get_own_property_descriptor<'gc>(a: &mut Agent<'gc, '_>, _: Value<'gc>, args: &[Value<'gc>]) -> Result<Value<'gc>, Error> {
        let o = Self::target(a, args.at(0), "getOwnPropertyDescriptor")?;
        let key = a.to_property_key(args.at(1))?;
        let p = a.get_own_property(o, key.key())?;
        Ok(a.from_property_descriptor(p))
    }

    fn get_prototype_of<'gc>(a: &mut Agent<'gc, '_>, _: Value<'gc>, args: &[Value<'gc>]) -> Result<Value<'gc>, Error> {
        let o = Self::target(a, args.at(0), "getPrototypeOf")?;
        Ok(Value::from_proto(a.get_prototype_of(o)?))
    }

    fn has<'gc>(a: &mut Agent<'gc, '_>, _: Value<'gc>, args: &[Value<'gc>]) -> Result<Value<'gc>, Error> {
        let o = Self::target(a, args.at(0), "has")?;
        let key = a.to_property_key(args.at(1))?;
        Ok(a.has_key(o, key.key())?.into())
    }

    fn is_extensible<'gc>(a: &mut Agent<'gc, '_>, _: Value<'gc>, args: &[Value<'gc>]) -> Result<Value<'gc>, Error> {
        let o = Self::target(a, args.at(0), "isExtensible")?;
        Ok(a.is_extensible(o)?.into())
    }

    fn own_keys<'gc>(a: &mut Agent<'gc, '_>, _: Value<'gc>, args: &[Value<'gc>]) -> Result<Value<'gc>, Error> {
        let o = Self::target(a, args.at(0), "ownKeys")?;
        let keys = a.own_property_keys(o)?.into_iter().map(Value::from).collect();
        Ok(a.array(keys).into())
    }

    fn prevent_extensions<'gc>(a: &mut Agent<'gc, '_>, _: Value<'gc>, args: &[Value<'gc>]) -> Result<Value<'gc>, Error> {
        let o = Self::target(a, args.at(0), "preventExtensions")?;
        Ok(a.prevent_extensions(o)?.into())
    }

    fn set<'gc>(a: &mut Agent<'gc, '_>, _: Value<'gc>, args: &[Value<'gc>]) -> Result<Value<'gc>, Error> {
        let o = Self::target(a, args.at(0), "set")?;
        let key = a.to_property_key(args.at(1))?;
        let receiver = if args.len() > 3 { args[3] } else { o.into() };
        Ok(a.object_set(o, key.key(), args.at(2), receiver)?.into())
    }

    fn set_prototype_of<'gc>(a: &mut Agent<'gc, '_>, _: Value<'gc>, args: &[Value<'gc>]) -> Result<Value<'gc>, Error> {
        let o = Self::target(a, args.at(0), "setPrototypeOf")?;
        let proto = match args.at(1).kind() {
            ValueKind::Object(p) => Some(p),
            ValueKind::Null => None,
            _ => return Err(a.throw(ErrorKind::Type, "Object prototype may only be an Object or null")),
        };
        Ok(a.set_prototype_of(o, proto)?.into())
    }
}

impl Installer<'_, '_> {
    pub(crate) fn reflect(&mut self) {
        let r = self.realm;
        // Proxy has no `prototype`
        let proxy = r.named_native(self.mc, Reflects::proxy, "Proxy", 2);
        if let ObjectKind::Native(n) = &mut proxy.borrow_mut(self.mc).kind {
            n.constructor = true;
        }
        self.method(proxy, "revocable", Reflects::revocable, 2);
        self.set(r.global, "Proxy", proxy.into());
        let reflect = self.plain();
        let methods: [(&str, j8framework::NativeFn, u32); 13] = [
            ("apply", Reflects::apply, 3),
            ("construct", Reflects::construct, 2),
            ("defineProperty", Reflects::define_property, 3),
            ("deleteProperty", Reflects::delete_property, 2),
            ("get", Reflects::get, 2),
            ("getOwnPropertyDescriptor", Reflects::get_own_property_descriptor, 2),
            ("getPrototypeOf", Reflects::get_prototype_of, 1),
            ("has", Reflects::has, 2),
            ("isExtensible", Reflects::is_extensible, 1),
            ("ownKeys", Reflects::own_keys, 1),
            ("preventExtensions", Reflects::prevent_extensions, 1),
            ("set", Reflects::set, 3),
            ("setPrototypeOf", Reflects::set_prototype_of, 2),
        ];
        for (name, f, n) in methods {
            self.method(reflect, name, f, n);
        }
        let tag = self.text("Reflect");
        reflect.borrow_mut(self.mc).define_symbol(r.symbols.to_string_tag, Property { slot: j8framework::Slot::Data(tag), enumerable: false, writable: false, configurable: true });
        self.set(r.global, "Reflect", reflect.into());
    }
}
