use alloc::vec::Vec;

use r8frame::{Agent, Bound, Error, ErrorKind, ObjectData, ObjectKind, Value};

use crate::{Args, Installer};

/// `Object` / `Function` / `Boolean` 与它们的原型方法。
struct Objects;

impl Objects {
    /// `Object(v)`：对象原样返回，否则给一个新空对象。
    fn object<'gc>(a: &mut Agent<'gc, '_>, _: Value<'gc>, args: &[Value<'gc>]) -> Result<Value<'gc>, Error> {
        Ok(match args.at(0) {
            v @ Value::Object(_) => v,
            _ => a.object().into(),
        })
    }

    /// 自有可枚举字符串键：数组下标在前，再按插入顺序。
    fn own_keys<'gc>(a: &mut Agent<'gc, '_>, v: Value<'gc>) -> Result<Vec<r8frame::JsString<'gc>>, Error> {
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
        keys.extend(data.props.iter().map(|&(k, _)| k));
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

    fn get_prototype_of<'gc>(_: &mut Agent<'gc, '_>, _: Value<'gc>, args: &[Value<'gc>]) -> Result<Value<'gc>, Error> {
        Ok(match args.at(0) {
            Value::Object(o) => o.borrow().proto.map_or(Value::Null, Value::Object),
            _ => Value::Null,
        })
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
        let tag = match this {
            Value::Undefined => "Undefined",
            Value::Null => "Null",
            Value::Object(o) => match o.borrow().kind {
                ObjectKind::Array(_) => "Array",
                ref k if k.is_callable() => "Function",
                _ => "Object",
            },
            Value::String(_) => "String",
            Value::Number(_) => "Number",
            Value::Bool(_) => "Boolean",
        };
        Ok(a.string(&alloc::format!("[object {tag}]")).into())
    }

    fn has_own_property<'gc>(a: &mut Agent<'gc, '_>, this: Value<'gc>, args: &[Value<'gc>]) -> Result<Value<'gc>, Error> {
        let k = a.to_string(args.at(0))?;
        let Value::Object(o) = this else { return Ok(false.into()) };
        let data = o.borrow();
        let own = data.own(&k).is_some() || matches!((&data.kind, k.parse::<usize>()), (ObjectKind::Array(items), Ok(i)) if i < items.len());
        Ok(own.into())
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
        self.constructor("Boolean", Objects::boolean, b);
        self.method(b, "valueOf", Objects::value_of);
    }
}
