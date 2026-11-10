use alloc::vec::Vec;

use r8frame::{Agent, Bound, Error, ErrorKind, ObjectData, ObjectKind, Property, Slot, Value};

use crate::{Args, Installer};

/// `Object` / `Function` / `Boolean` 与它们的原型方法。
struct Objects;

impl Objects {
    /// `Object(v)`：对象原样返回，否则给一个新空对象。
    fn object<'gc>(a: &mut Agent<'gc, '_>, _: Value<'gc>, args: &[Value<'gc>]) -> Result<Value<'gc>, Error> {
        Ok(match args.at(0) {
            Value::Undefined | Value::Null | Value::Uninit => a.object().into(),
            v => a.to_object(v)?.into(),
        })
    }

    /// 自有可枚举字符串键：数组下标在前，再按插入顺序。
    fn own_keys<'gc>(a: &mut Agent<'gc, '_>, v: Value<'gc>) -> Result<Vec<r8frame::JsString<'gc>>, Error> {
        Self::keys_where(a, v, true)
    }

    /// 自有字符串键（`enumerable_only` 时只要可枚举的）。
    fn keys_where<'gc>(a: &mut Agent<'gc, '_>, v: Value<'gc>, enumerable_only: bool) -> Result<Vec<r8frame::JsString<'gc>>, Error> {
        let Value::Object(o) = v else {
            return Err(a.throw(ErrorKind::Type, "Object.keys called on non-object"));
        };
        let data = o.borrow();
        let mut keys = Vec::new();
        if let ObjectKind::Array(items) = &data.kind {
            for i in 0..items.len() {
                keys.push(a.string(ryu_js::Buffer::new().format(f64::from(u32::try_from(i).unwrap_or(u32::MAX)))));
            }
        }
        keys.extend(data.props.iter().filter(|(_, p)| p.enumerable || !enumerable_only).map(|&(k, _)| k));
        Ok(keys)
    }

    fn keys<'gc>(a: &mut Agent<'gc, '_>, _: Value<'gc>, args: &[Value<'gc>]) -> Result<Value<'gc>, Error> {
        let keys = Self::own_keys(a, args.at(0))?.into_iter().map(Value::String).collect();
        Ok(a.array(keys).into())
    }

    fn values<'gc>(a: &mut Agent<'gc, '_>, _: Value<'gc>, args: &[Value<'gc>]) -> Result<Value<'gc>, Error> {
        let o = args.at(0);
        let keys = Self::own_keys(a, o)?;
        let values = keys.into_iter().map(|k| a.get_named(o, &k)).collect::<Result<_, _>>()?;
        Ok(a.array(values).into())
    }

    fn entries<'gc>(a: &mut Agent<'gc, '_>, _: Value<'gc>, args: &[Value<'gc>]) -> Result<Value<'gc>, Error> {
        let o = args.at(0);
        let keys = Self::own_keys(a, o)?;
        let mut out = Vec::with_capacity(keys.len());
        for k in keys {
            let v = a.get_named(o, &k)?;
            out.push(a.array(alloc::vec![Value::String(k), v]).into());
        }
        Ok(a.array(out).into())
    }

    /// `Object.assign(target, ...sources)`。
    fn assign<'gc>(a: &mut Agent<'gc, '_>, _: Value<'gc>, args: &[Value<'gc>]) -> Result<Value<'gc>, Error> {
        let target = args.at(0);
        for &src in args.iter().skip(1).filter(|v| matches!(v, Value::Object(_))) {
            for k in Self::own_keys(a, src)? {
                let v = a.get_named(src, &k)?;
                a.set_named(target, &k, v)?;
            }
        }
        Ok(target)
    }

    /// `Object.getPrototypeOf(v)`：先 ToObject（原始值给它那一类的原型，`undefined` / `null` 抛 TypeError）。
    fn get_prototype_of<'gc>(a: &mut Agent<'gc, '_>, _: Value<'gc>, args: &[Value<'gc>]) -> Result<Value<'gc>, Error> {
        let o = a.to_object(args.at(0))?;
        Ok(o.borrow().proto.map_or(Value::Null, Value::Object))
    }

    /// `Object.create(proto)`。
    fn create<'gc>(a: &mut Agent<'gc, '_>, _: Value<'gc>, args: &[Value<'gc>]) -> Result<Value<'gc>, Error> {
        let proto = match args.at(0) {
            Value::Object(o) => Some(o),
            Value::Null => None,
            _ => return Err(a.throw(ErrorKind::Type, "Object prototype may only be an Object or null")),
        };
        Ok(ObjectData { proto, ..ObjectData::default() }.alloc(a.mc).into())
    }

    fn to_string<'gc>(a: &mut Agent<'gc, '_>, this: Value<'gc>, _: &[Value<'gc>]) -> Result<Value<'gc>, Error> {
        // 包装对象按里面的原始值给标签（`[object Boolean]` …）
        let tag = match this.unwrapped() {
            Value::Undefined | Value::Uninit => "Undefined",
            Value::Null => "Null",
            Value::Object(o) => match o.borrow().kind {
                ObjectKind::Array(_) => "Array",
                ObjectKind::RegExp(_) => "RegExp",
                ref k if k.is_callable() => "Function",
                _ => "Object",
            },
            Value::String(_) => "String",
            Value::Number(_) => "Number",
            Value::Bool(_) => "Boolean",
            Value::Symbol(_) => "Symbol",
            Value::BigInt(_) => "BigInt",
        };
        Ok(a.string(&alloc::format!("[object {tag}]")).into())
    }

    fn has_own_property<'gc>(a: &mut Agent<'gc, '_>, this: Value<'gc>, args: &[Value<'gc>]) -> Result<Value<'gc>, Error> {
        let k = a.to_string(args.at(0))?;
        let Value::Object(o) = this else { return Ok(false.into()) };
        let data = o.borrow();
        let own = data.get_own(&k).is_some() || matches!((&data.kind, k.parse::<usize>()), (ObjectKind::Array(items), Ok(i)) if i < items.len());
        Ok(own.into())
    }

    fn get_own_property_names<'gc>(a: &mut Agent<'gc, '_>, _: Value<'gc>, args: &[Value<'gc>]) -> Result<Value<'gc>, Error> {
        let mut keys: Vec<Value<'gc>> = Self::keys_where(a, args.at(0), false)?.into_iter().map(Value::String).collect();
        if let Value::Object(o) = args.at(0)
            && matches!(o.borrow().kind, ObjectKind::Array(_))
        {
            keys.push(a.string("length").into());
        }
        Ok(a.array(keys).into())
    }

    /// 描述符对象里的一个布尔字段（缺省 `false`）。
    fn flag<'gc>(a: &mut Agent<'gc, '_>, desc: Value<'gc>, name: &str) -> Result<bool, Error> {
        let v = a.get_named(desc, name)?;
        Ok(a.to_boolean(v))
    }

    /// 描述符对象里的访问器（`undefined` 给 `None`，不可调抛 TypeError）。
    fn accessor<'gc>(a: &mut Agent<'gc, '_>, desc: Value<'gc>, name: &str) -> Result<Option<r8frame::JsObject<'gc>>, Error> {
        match a.get_named(desc, name)? {
            Value::Undefined => Ok(None),
            Value::Object(f) if f.borrow().kind.is_callable() => Ok(Some(f)),
            _ => Err(a.throw(ErrorKind::Type, "Getter / setter must be a function")),
        }
    }

    /// `Object.defineProperty(o, key, descriptor)`。
    fn define_property<'gc>(a: &mut Agent<'gc, '_>, _: Value<'gc>, args: &[Value<'gc>]) -> Result<Value<'gc>, Error> {
        let (target, desc) = (args.at(0), args.at(2));
        let (Value::Object(o), Value::Object(_)) = (target, desc) else {
            return Err(a.throw(ErrorKind::Type, "Object.defineProperty called on non-object"));
        };
        let key = a.property_key(args.at(1))?;
        let enumerable = Self::flag(a, desc, "enumerable")?;
        let configurable = Self::flag(a, desc, "configurable")?;
        let has_accessor = a.has_property(desc, "get")? || a.has_property(desc, "set")?;
        let prop = if has_accessor {
            let get = Self::accessor(a, desc, "get")?;
            let set = Self::accessor(a, desc, "set")?;
            Property { slot: Slot::Accessor { get, set }, enumerable, writable: false, configurable }
        } else {
            let value = a.get_named(desc, "value")?;
            let writable = Self::flag(a, desc, "writable")?;
            Property { slot: Slot::Data(value), enumerable, writable, configurable }
        };
        a.define(o, &key, prop);
        Ok(target)
    }

    /// `Object.getOwnPropertyDescriptor(o, key)`。
    fn get_own_property_descriptor<'gc>(a: &mut Agent<'gc, '_>, _: Value<'gc>, args: &[Value<'gc>]) -> Result<Value<'gc>, Error> {
        let Value::Object(o) = args.at(0) else { return Ok(Value::Undefined) };
        let key = a.property_key(args.at(1))?;
        let Some(p) = o.borrow().get_own(&key) else { return Ok(Value::Undefined) };
        let d = a.object();
        let set = |name: &str, v: Value<'gc>| d.borrow_mut(a.mc).set(a.mc, name, v);
        match p.slot {
            Slot::Data(v) => {
                set("value", v);
                set("writable", p.writable.into());
            }
            Slot::Accessor { get, set: s } => {
                set("get", get.map_or(Value::Undefined, Value::Object));
                set("set", s.map_or(Value::Undefined, Value::Object));
            }
        }
        set("enumerable", p.enumerable.into());
        set("configurable", p.configurable.into());
        Ok(d.into())
    }

    /// `Object.freeze(o)`：全部属性不可写、不可配置（还没有不可扩展）。
    fn freeze<'gc>(a: &mut Agent<'gc, '_>, _: Value<'gc>, args: &[Value<'gc>]) -> Result<Value<'gc>, Error> {
        if let Value::Object(o) = args.at(0) {
            for (_, p) in &mut o.borrow_mut(a.mc).props {
                p.writable = false;
                p.configurable = false;
            }
        }
        Ok(args.at(0))
    }

    fn value_of<'gc>(_: &mut Agent<'gc, '_>, this: Value<'gc>, _: &[Value<'gc>]) -> Result<Value<'gc>, Error> {
        Ok(this)
    }

    /// `f.call(this, ...args)`。
    fn call<'gc>(a: &mut Agent<'gc, '_>, this: Value<'gc>, args: &[Value<'gc>]) -> Result<Value<'gc>, Error> {
        a.call(this, args.at(0), args.get(1..).unwrap_or(&[]))
    }

    /// `f.apply(this, array)`。
    fn apply<'gc>(a: &mut Agent<'gc, '_>, this: Value<'gc>, args: &[Value<'gc>]) -> Result<Value<'gc>, Error> {
        let list = match args.at(1) {
            Value::Undefined | Value::Null => Vec::new(),
            Value::Object(o) => match &o.borrow().kind {
                ObjectKind::Array(items) => items.clone(),
                _ => return Err(a.throw(ErrorKind::Type, "CreateListFromArrayLike called on non-array")),
            },
            _ => return Err(a.throw(ErrorKind::Type, "CreateListFromArrayLike called on non-object")),
        };
        a.call(this, args.at(0), &list)
    }

    /// `f.bind(this, ...args)`。
    fn bind<'gc>(a: &mut Agent<'gc, '_>, this: Value<'gc>, args: &[Value<'gc>]) -> Result<Value<'gc>, Error> {
        if !a.is_callable(this) {
            return Err(a.throw(ErrorKind::Type, "Bind must be called on a function"));
        }
        let bound = Bound { target: this, this: args.at(0), args: args.get(1..).unwrap_or(&[]).to_vec() };
        Ok(ObjectData { kind: ObjectKind::Bound(bound), proto: Some(a.realm.function_proto), ..ObjectData::default() }.alloc(a.mc).into())
    }

    fn function_to_string<'gc>(a: &mut Agent<'gc, '_>, _: Value<'gc>, _: &[Value<'gc>]) -> Result<Value<'gc>, Error> {
        Ok(a.string("function () { [native code] }").into())
    }

    fn boolean<'gc>(a: &mut Agent<'gc, '_>, _: Value<'gc>, args: &[Value<'gc>]) -> Result<Value<'gc>, Error> {
        Ok(a.to_boolean(args.at(0)).into())
    }

    /// thisBooleanValue：布尔或 Boolean 包装对象，别的抛 TypeError。
    fn this_boolean<'gc>(a: &Agent<'gc, '_>, this: Value<'gc>, method: &str) -> Result<bool, Error> {
        match this.unwrapped() {
            Value::Bool(b) => Ok(b),
            _ => Err(a.throw(ErrorKind::Type, &alloc::format!("Boolean.prototype.{method} requires that 'this' be a Boolean"))),
        }
    }

    fn boolean_to_string<'gc>(a: &mut Agent<'gc, '_>, this: Value<'gc>, _: &[Value<'gc>]) -> Result<Value<'gc>, Error> {
        let b = Self::this_boolean(a, this, "toString")?;
        Ok(a.string(if b { "true" } else { "false" }).into())
    }

    fn boolean_value_of<'gc>(a: &mut Agent<'gc, '_>, this: Value<'gc>, _: &[Value<'gc>]) -> Result<Value<'gc>, Error> {
        Ok(Self::this_boolean(a, this, "valueOf")?.into())
    }
}

impl Installer<'_, '_> {
    pub(crate) fn object(&mut self) {
        let proto = self.realm.object_proto;
        let ctor = self.constructor("Object", Objects::object, proto);
        self.method(ctor, "keys", Objects::keys);
        self.method(ctor, "values", Objects::values);
        self.method(ctor, "entries", Objects::entries);
        self.method(ctor, "assign", Objects::assign);
        self.method(ctor, "getPrototypeOf", Objects::get_prototype_of);
        self.method(ctor, "create", Objects::create);
        self.method(ctor, "getOwnPropertyNames", Objects::get_own_property_names);
        self.method(ctor, "defineProperty", Objects::define_property);
        self.method(ctor, "getOwnPropertyDescriptor", Objects::get_own_property_descriptor);
        self.method(ctor, "freeze", Objects::freeze);
        self.method(proto, "toString", Objects::to_string);
        self.method(proto, "hasOwnProperty", Objects::has_own_property);
        self.method(proto, "valueOf", Objects::value_of);
    }

    pub(crate) fn function(&mut self) {
        let proto = self.realm.function_proto;
        self.method(proto, "call", Objects::call);
        self.method(proto, "apply", Objects::apply);
        self.method(proto, "bind", Objects::bind);
        self.method(proto, "toString", Objects::function_to_string);
        let b = self.realm.boolean_proto;
        // Boolean.prototype 本身是包着 false 的 Boolean 对象
        b.borrow_mut(self.mc).kind = ObjectKind::Primitive(false.into());
        self.constructor("Boolean", Objects::boolean, b);
        self.method(b, "toString", Objects::boolean_to_string);
        self.method(b, "valueOf", Objects::boolean_value_of);
    }
}
