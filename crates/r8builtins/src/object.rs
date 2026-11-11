use alloc::vec::Vec;

use r8frame::{Agent, Bound, Descriptor, Error, ErrorKind, Integrity, IterKind, JsObject, Key, ObjectData, ObjectKind, Property, PropertyKey, Slot, Value};

use crate::{Args, Installer};

/// `Object` / `Function` / `Boolean` 与它们的原型方法。
struct Objects;

impl Objects {
    /// `new.target` 是不是 `Object` 自己。
    fn is_object_ctor(v: Value<'_>) -> bool {
        matches!(v, Value::Object(f) if matches!(f.borrow().kind, ObjectKind::Native(n) if core::ptr::fn_addr_eq(n.f, Self::object as r8frame::NativeFn)))
    }

    /// `Object(v)`：子类构造时按 `new.target` 建；否则对象原样返回、`undefined` / `null` 给新空对象、原始值包装。
    fn object<'gc>(a: &mut Agent<'gc, '_>, _: Value<'gc>, args: &[Value<'gc>]) -> Result<Value<'gc>, Error> {
        if a.new_target != Value::Undefined && !Self::is_object_ctor(a.new_target) {
            return Ok(a.create_from_ctor(a.realm.object_proto, ObjectKind::Ordinary)?.into());
        }
        Ok(match args.at(0) {
            Value::Undefined | Value::Null | Value::Uninit => a.object().into(),
            v => a.to_object(v)?.into(),
        })
    }

    /// 要求是对象（`Object.defineProperty` 等）。
    fn require_object<'gc>(a: &mut Agent<'gc, '_>, v: Value<'gc>, method: &str) -> Result<JsObject<'gc>, Error> {
        match v {
            Value::Object(o) => Ok(o),
            _ => Err(a.throw(ErrorKind::Type, &alloc::format!("Object.{method} called on non-object"))),
        }
    }

    /// EnumerableOwnProperties 的三种。
    fn enumerable_own<'gc>(a: &mut Agent<'gc, '_>, v: Value<'gc>, kind: IterKind) -> Result<Value<'gc>, Error> {
        let o = a.to_object(v)?;
        let mut out = Vec::new();
        for k in a.own_property_keys(o)? {
            let PropertyKey::String(s) = k else { continue };
            if !a.get_own_property(o, Key::Str(&s))?.is_some_and(|p| p.enumerable) {
                continue;
            }
            out.push(match kind {
                IterKind::Keys => s.into(),
                IterKind::Values => a.get_named(o.into(), &s)?,
                IterKind::Entries => {
                    let v = a.get_named(o.into(), &s)?;
                    a.array(alloc::vec![s.into(), v]).into()
                }
            });
        }
        Ok(a.array(out).into())
    }

    fn keys<'gc>(a: &mut Agent<'gc, '_>, _: Value<'gc>, args: &[Value<'gc>]) -> Result<Value<'gc>, Error> {
        Self::enumerable_own(a, args.at(0), IterKind::Keys)
    }

    fn values<'gc>(a: &mut Agent<'gc, '_>, _: Value<'gc>, args: &[Value<'gc>]) -> Result<Value<'gc>, Error> {
        Self::enumerable_own(a, args.at(0), IterKind::Values)
    }

    fn entries<'gc>(a: &mut Agent<'gc, '_>, _: Value<'gc>, args: &[Value<'gc>]) -> Result<Value<'gc>, Error> {
        Self::enumerable_own(a, args.at(0), IterKind::Entries)
    }

    /// `Object.assign(target, ...sources)`。
    fn assign<'gc>(a: &mut Agent<'gc, '_>, _: Value<'gc>, args: &[Value<'gc>]) -> Result<Value<'gc>, Error> {
        let to = a.to_object(args.at(0))?;
        for &src in args.iter().skip(1).filter(|v| !v.is_nullish()) {
            let from = a.to_object(src)?;
            for k in a.own_property_keys(from)? {
                let key = k.key();
                if a.get_own_property(from, key)?.is_some_and(|p| p.enumerable) {
                    let v = a.get_key(from.into(), key, from.into())?;
                    if !a.set_key(to.into(), key, v, to.into())? {
                        let t = a.key_text(key);
                        return Err(a.throw(ErrorKind::Type, &alloc::format!("Cannot assign to read only property '{t}' of object")));
                    }
                }
            }
        }
        Ok(to.into())
    }

    fn get_prototype_of<'gc>(a: &mut Agent<'gc, '_>, _: Value<'gc>, args: &[Value<'gc>]) -> Result<Value<'gc>, Error> {
        let o = a.to_object(args.at(0))?;
        Ok(a.get_prototype_of(o)?.map_or(Value::Null, Value::Object))
    }

    /// 原型实参：对象或 `null`。
    fn proto_arg<'gc>(a: &mut Agent<'gc, '_>, v: Value<'gc>) -> Result<Option<JsObject<'gc>>, Error> {
        match v {
            Value::Object(o) => Ok(Some(o)),
            Value::Null => Ok(None),
            other => {
                let t = a.describe(other);
                Err(a.throw(ErrorKind::Type, &alloc::format!("Object prototype may only be an Object or null: {t}")))
            }
        }
    }

    fn set_prototype_of<'gc>(a: &mut Agent<'gc, '_>, _: Value<'gc>, args: &[Value<'gc>]) -> Result<Value<'gc>, Error> {
        let target = args.at(0);
        if target.is_nullish() {
            return Err(a.throw(ErrorKind::Type, "Object.setPrototypeOf called on null or undefined"));
        }
        let proto = Self::proto_arg(a, args.at(1))?;
        let Value::Object(o) = target else { return Ok(target) };
        if !a.set_prototype_of(o, proto)? {
            return Err(a.throw(ErrorKind::Type, "Cannot set prototype: object is not extensible or the chain would be cyclic"));
        }
        Ok(target)
    }

    /// ObjectDefineProperties。
    fn define_properties_on<'gc>(a: &mut Agent<'gc, '_>, o: JsObject<'gc>, props: Value<'gc>) -> Result<(), Error> {
        let props = a.to_object(props)?;
        let mut descs = Vec::new();
        for k in a.own_property_keys(props)? {
            if a.get_own_property(props, k.key())?.is_some_and(|p| p.enumerable) {
                let d = a.get_key(props.into(), k.key(), props.into())?;
                descs.push((k, a.to_property_descriptor(d)?));
            }
        }
        for (k, d) in descs {
            a.define_property_or_throw(o, k.key(), d)?;
        }
        Ok(())
    }

    fn create<'gc>(a: &mut Agent<'gc, '_>, _: Value<'gc>, args: &[Value<'gc>]) -> Result<Value<'gc>, Error> {
        let proto = Self::proto_arg(a, args.at(0))?;
        let o = ObjectData { proto, ..ObjectData::default() }.alloc(a.mc);
        if args.at(1) != Value::Undefined {
            Self::define_properties_on(a, o, args.at(1))?;
        }
        Ok(o.into())
    }

    fn define_properties<'gc>(a: &mut Agent<'gc, '_>, _: Value<'gc>, args: &[Value<'gc>]) -> Result<Value<'gc>, Error> {
        let o = Self::require_object(a, args.at(0), "defineProperties")?;
        Self::define_properties_on(a, o, args.at(1))?;
        Ok(o.into())
    }

    fn define_property<'gc>(a: &mut Agent<'gc, '_>, _: Value<'gc>, args: &[Value<'gc>]) -> Result<Value<'gc>, Error> {
        let o = Self::require_object(a, args.at(0), "defineProperty")?;
        let key = a.to_property_key(args.at(1))?;
        let desc = a.to_property_descriptor(args.at(2))?;
        a.define_property_or_throw(o, key.key(), desc)?;
        Ok(o.into())
    }

    fn get_own_property_descriptor<'gc>(a: &mut Agent<'gc, '_>, _: Value<'gc>, args: &[Value<'gc>]) -> Result<Value<'gc>, Error> {
        let o = a.to_object(args.at(0))?;
        let key = a.to_property_key(args.at(1))?;
        let p = a.get_own_property(o, key.key())?;
        Ok(a.from_property_descriptor(p))
    }

    fn get_own_property_descriptors<'gc>(a: &mut Agent<'gc, '_>, _: Value<'gc>, args: &[Value<'gc>]) -> Result<Value<'gc>, Error> {
        let o = a.to_object(args.at(0))?;
        let out = a.object();
        for k in a.own_property_keys(o)? {
            let p = a.get_own_property(o, k.key())?;
            let d = a.from_property_descriptor(p);
            if d != Value::Undefined {
                a.create_data_property_or_throw(out, k.key(), d)?;
            }
        }
        Ok(out.into())
    }

    /// GetOwnPropertyKeys：只要字符串键 / 只要 Symbol 键。
    fn own_keys_of<'gc>(a: &mut Agent<'gc, '_>, v: Value<'gc>, symbols: bool) -> Result<Value<'gc>, Error> {
        let o = a.to_object(v)?;
        let keys = a.own_property_keys(o)?.into_iter().filter(|k| matches!(k, PropertyKey::Symbol(_)) == symbols).map(Value::from).collect();
        Ok(a.array(keys).into())
    }

    fn get_own_property_names<'gc>(a: &mut Agent<'gc, '_>, _: Value<'gc>, args: &[Value<'gc>]) -> Result<Value<'gc>, Error> {
        Self::own_keys_of(a, args.at(0), false)
    }

    fn get_own_property_symbols<'gc>(a: &mut Agent<'gc, '_>, _: Value<'gc>, args: &[Value<'gc>]) -> Result<Value<'gc>, Error> {
        Self::own_keys_of(a, args.at(0), true)
    }

    fn freeze<'gc>(a: &mut Agent<'gc, '_>, _: Value<'gc>, args: &[Value<'gc>]) -> Result<Value<'gc>, Error> {
        Self::integrity(a, args.at(0), Integrity::Frozen)
    }

    fn seal<'gc>(a: &mut Agent<'gc, '_>, _: Value<'gc>, args: &[Value<'gc>]) -> Result<Value<'gc>, Error> {
        Self::integrity(a, args.at(0), Integrity::Sealed)
    }

    fn integrity<'gc>(a: &mut Agent<'gc, '_>, v: Value<'gc>, level: Integrity) -> Result<Value<'gc>, Error> {
        if let Value::Object(o) = v
            && !a.set_integrity_level(o, level)?
        {
            return Err(a.throw(ErrorKind::Type, "Cannot freeze or seal the object"));
        }
        Ok(v)
    }

    fn prevent_extensions<'gc>(a: &mut Agent<'gc, '_>, _: Value<'gc>, args: &[Value<'gc>]) -> Result<Value<'gc>, Error> {
        if let Value::Object(o) = args.at(0)
            && !a.prevent_extensions(o)?
        {
            return Err(a.throw(ErrorKind::Type, "Cannot prevent extensions"));
        }
        Ok(args.at(0))
    }

    fn is_extensible<'gc>(a: &mut Agent<'gc, '_>, _: Value<'gc>, args: &[Value<'gc>]) -> Result<Value<'gc>, Error> {
        Ok(match args.at(0) {
            Value::Object(o) => a.is_extensible(o)?,
            _ => false,
        }
        .into())
    }

    fn is_frozen<'gc>(a: &mut Agent<'gc, '_>, _: Value<'gc>, args: &[Value<'gc>]) -> Result<Value<'gc>, Error> {
        Ok(match args.at(0) {
            Value::Object(o) => a.test_integrity_level(o, Integrity::Frozen)?,
            _ => true,
        }
        .into())
    }

    fn is_sealed<'gc>(a: &mut Agent<'gc, '_>, _: Value<'gc>, args: &[Value<'gc>]) -> Result<Value<'gc>, Error> {
        Ok(match args.at(0) {
            Value::Object(o) => a.test_integrity_level(o, Integrity::Sealed)?,
            _ => true,
        }
        .into())
    }

    fn is<'gc>(_: &mut Agent<'gc, '_>, _: Value<'gc>, args: &[Value<'gc>]) -> Result<Value<'gc>, Error> {
        Ok(args.at(0).same_value(args.at(1)).into())
    }

    fn has_own<'gc>(a: &mut Agent<'gc, '_>, _: Value<'gc>, args: &[Value<'gc>]) -> Result<Value<'gc>, Error> {
        let o = a.to_object(args.at(0))?;
        let key = a.to_property_key(args.at(1))?;
        Ok(a.has_own_property(o, key.key())?.into())
    }

    /// `Object.fromEntries(iterable)`。
    fn from_entries<'gc>(a: &mut Agent<'gc, '_>, _: Value<'gc>, args: &[Value<'gc>]) -> Result<Value<'gc>, Error> {
        let iterable = args.at(0);
        if iterable.is_nullish() {
            return Err(a.throw(ErrorKind::Type, "Object.fromEntries requires an iterable"));
        }
        let o = a.object();
        let it = a.get_iterator(iterable)?;
        while let Some(item) = a.iterator_step(it)? {
            let r = (|| {
                if !matches!(item, Value::Object(_)) {
                    let t = a.describe(item);
                    return Err(a.throw(ErrorKind::Type, &alloc::format!("Iterator value {t} is not an entry object")));
                }
                let k = a.get_index(item, 0.0)?;
                let v = a.get_index(item, 1.0)?;
                let key = a.to_property_key(k)?;
                a.create_data_property_or_throw(o, key.key(), v)
            })();
            if let Err(e) = r {
                let _ = a.iterator_close(it.iterator);
                return Err(e);
            }
        }
        Ok(o.into())
    }

    /// `Object.groupBy(items, callback)`。
    fn group_by<'gc>(a: &mut Agent<'gc, '_>, _: Value<'gc>, args: &[Value<'gc>]) -> Result<Value<'gc>, Error> {
        let groups = Self::group(a, args.at(0), args.at(1), true)?;
        let o = ObjectData::default().alloc(a.mc);
        for (k, items) in groups {
            let arr = a.array(items);
            let key = a.to_property_key(k)?;
            a.create_data_property_or_throw(o, key.key(), arr.into())?;
        }
        Ok(o.into())
    }

    /// GroupBy：键按出现顺序分组（`property` 时键先 ToPropertyKey，否则 `-0` 规整成 `+0`）。
    pub(crate) fn group<'gc>(a: &mut Agent<'gc, '_>, items: Value<'gc>, callback: Value<'gc>, property: bool) -> Result<Vec<(Value<'gc>, Vec<Value<'gc>>)>, Error> {
        if items.is_nullish() {
            return Err(a.throw(ErrorKind::Type, "groupBy called on null or undefined"));
        }
        if !a.is_callable(callback) {
            return Err(a.throw(ErrorKind::Type, "groupBy callback is not a function"));
        }
        let mut groups: Vec<(Value<'gc>, Vec<Value<'gc>>)> = Vec::new();
        let it = a.get_iterator(items)?;
        let mut k = 0.0;
        while let Some(v) = a.iterator_step(it)? {
            let key = a.call(callback, Value::Undefined, &[v, k.into()]).and_then(|key| {
                if property {
                    a.to_property_key(key).map(Value::from)
                } else {
                    Ok(match key {
                        Value::Number(0.0) => Value::Number(0.0),
                        other => other,
                    })
                }
            });
            let key = match key {
                Ok(key) => key,
                Err(e) => {
                    let _ = a.iterator_close(it.iterator);
                    return Err(e);
                }
            };
            match groups.iter_mut().find(|(g, _)| g.same_value(key)) {
                Some((_, list)) => list.push(v),
                None => groups.push((key, alloc::vec![v])),
            }
            k += 1.0;
        }
        Ok(groups)
    }

    fn to_string<'gc>(a: &mut Agent<'gc, '_>, this: Value<'gc>, _: &[Value<'gc>]) -> Result<Value<'gc>, Error> {
        let builtin = match this {
            Value::Undefined | Value::Uninit => return Ok(a.string("[object Undefined]").into()),
            Value::Null => return Ok(a.string("[object Null]").into()),
            _ => {
                let o = a.to_object(this)?;
                let is_array = a.is_array(o.into())?;
                let tag = match &o.borrow().kind {
                    _ if is_array => "Array",
                    ObjectKind::Arguments => "Arguments",
                    k if k.is_callable() => "Function",
                    ObjectKind::Error => "Error",
                    ObjectKind::Primitive(Value::Bool(_)) => "Boolean",
                    ObjectKind::Primitive(Value::Number(_)) => "Number",
                    ObjectKind::Primitive(Value::String(_)) => "String",
                    ObjectKind::RegExp(_) => "RegExp",
                    _ => "Object",
                };
                let t = a.get_symbol(o.into(), a.realm.symbols.to_string_tag, o.into())?;
                if let Value::String(s) = t {
                    return Ok(a.string(&alloc::format!("[object {}]", &**s)).into());
                }
                tag
            }
        };
        Ok(a.string(&alloc::format!("[object {builtin}]")).into())
    }

    fn to_locale_string<'gc>(a: &mut Agent<'gc, '_>, this: Value<'gc>, _: &[Value<'gc>]) -> Result<Value<'gc>, Error> {
        let f = a.get_named(this, "toString")?;
        a.call(f, this, &[])
    }

    fn has_own_property<'gc>(a: &mut Agent<'gc, '_>, this: Value<'gc>, args: &[Value<'gc>]) -> Result<Value<'gc>, Error> {
        let key = a.to_property_key(args.at(0))?;
        let o = a.to_object(this)?;
        Ok(a.has_own_property(o, key.key())?.into())
    }

    fn is_prototype_of<'gc>(a: &mut Agent<'gc, '_>, this: Value<'gc>, args: &[Value<'gc>]) -> Result<Value<'gc>, Error> {
        let Value::Object(v) = args.at(0) else { return Ok(false.into()) };
        let o = a.to_object(this)?;
        let mut cur = a.get_prototype_of(v)?;
        while let Some(c) = cur {
            if gc_arena::Gc::ptr_eq(c, o) {
                return Ok(true.into());
            }
            cur = a.get_prototype_of(c)?;
        }
        Ok(false.into())
    }

    fn property_is_enumerable<'gc>(a: &mut Agent<'gc, '_>, this: Value<'gc>, args: &[Value<'gc>]) -> Result<Value<'gc>, Error> {
        let key = a.to_property_key(args.at(0))?;
        let o = a.to_object(this)?;
        Ok(a.get_own_property(o, key.key())?.is_some_and(|p| p.enumerable).into())
    }

    fn proto_getter<'gc>(a: &mut Agent<'gc, '_>, this: Value<'gc>, _: &[Value<'gc>]) -> Result<Value<'gc>, Error> {
        let o = a.to_object(this)?;
        Ok(a.get_prototype_of(o)?.map_or(Value::Null, Value::Object))
    }

    fn proto_setter<'gc>(a: &mut Agent<'gc, '_>, this: Value<'gc>, args: &[Value<'gc>]) -> Result<Value<'gc>, Error> {
        if this.is_nullish() {
            return Err(a.throw(ErrorKind::Type, "Object.prototype.__proto__ called on null or undefined"));
        }
        let proto = match args.at(0) {
            Value::Object(p) => Some(p),
            Value::Null => None,
            _ => return Ok(Value::Undefined),
        };
        let Value::Object(o) = this else { return Ok(Value::Undefined) };
        if !a.set_prototype_of(o, proto)? {
            return Err(a.throw(ErrorKind::Type, "Cyclic __proto__ value or object is not extensible"));
        }
        Ok(Value::Undefined)
    }

    /// `__defineGetter__` / `__defineSetter__`（annexB）。
    fn define_half<'gc>(a: &mut Agent<'gc, '_>, this: Value<'gc>, args: &[Value<'gc>], setter: bool) -> Result<Value<'gc>, Error> {
        let o = a.to_object(this)?;
        let f = match args.at(1) {
            Value::Object(f) if f.borrow().kind.is_callable() => f,
            _ => return Err(a.throw(ErrorKind::Type, "Object.prototype.__defineGetter__: Expecting function")),
        };
        let desc = if setter { Descriptor { set: Some(Some(f)), ..Descriptor::default() } } else { Descriptor { get: Some(Some(f)), ..Descriptor::default() } };
        let desc = Descriptor { enumerable: Some(true), configurable: Some(true), ..desc };
        let key = a.to_property_key(args.at(0))?;
        a.define_property_or_throw(o, key.key(), desc)?;
        Ok(Value::Undefined)
    }

    fn define_getter<'gc>(a: &mut Agent<'gc, '_>, this: Value<'gc>, args: &[Value<'gc>]) -> Result<Value<'gc>, Error> {
        Self::define_half(a, this, args, false)
    }

    fn define_setter<'gc>(a: &mut Agent<'gc, '_>, this: Value<'gc>, args: &[Value<'gc>]) -> Result<Value<'gc>, Error> {
        Self::define_half(a, this, args, true)
    }

    /// `__lookupGetter__` / `__lookupSetter__`（annexB）。
    fn lookup_half<'gc>(a: &mut Agent<'gc, '_>, this: Value<'gc>, args: &[Value<'gc>], setter: bool) -> Result<Value<'gc>, Error> {
        let o = a.to_object(this)?;
        let key = a.to_property_key(args.at(0))?;
        let mut cur = Some(o);
        while let Some(c) = cur {
            if let Some(p) = a.get_own_property(c, key.key())? {
                return Ok(match p.slot {
                    Slot::Accessor { get, set } => (if setter { set } else { get }).map_or(Value::Undefined, Value::Object),
                    Slot::Data(_) => Value::Undefined,
                });
            }
            cur = a.get_prototype_of(c)?;
        }
        Ok(Value::Undefined)
    }

    fn lookup_getter<'gc>(a: &mut Agent<'gc, '_>, this: Value<'gc>, args: &[Value<'gc>]) -> Result<Value<'gc>, Error> {
        Self::lookup_half(a, this, args, false)
    }

    fn lookup_setter<'gc>(a: &mut Agent<'gc, '_>, this: Value<'gc>, args: &[Value<'gc>]) -> Result<Value<'gc>, Error> {
        Self::lookup_half(a, this, args, true)
    }

    fn value_of<'gc>(a: &mut Agent<'gc, '_>, this: Value<'gc>, _: &[Value<'gc>]) -> Result<Value<'gc>, Error> {
        Ok(a.to_object(this)?.into())
    }

    /// `f.call(this, ...args)`。
    fn call<'gc>(a: &mut Agent<'gc, '_>, this: Value<'gc>, args: &[Value<'gc>]) -> Result<Value<'gc>, Error> {
        if !a.is_callable(this) {
            return Err(a.throw(ErrorKind::Type, "Function.prototype.call called on a non-function"));
        }
        a.call(this, args.at(0), args.get(1..).unwrap_or(&[]))
    }

    /// `f.apply(this, arrayLike)`。
    fn apply<'gc>(a: &mut Agent<'gc, '_>, this: Value<'gc>, args: &[Value<'gc>]) -> Result<Value<'gc>, Error> {
        if !a.is_callable(this) {
            return Err(a.throw(ErrorKind::Type, "Function.prototype.apply was called on a non-function"));
        }
        let list = match args.at(1) {
            Value::Undefined | Value::Null => Vec::new(),
            v => a.list_from_array_like(v)?,
        };
        a.call(this, args.at(0), &list)
    }

    /// `f.bind(this, ...args)`：`length` = max(0, 目标的 length − 预先绑定的个数)，`name` = "bound " + 目标的 name。
    fn bind<'gc>(a: &mut Agent<'gc, '_>, this: Value<'gc>, args: &[Value<'gc>]) -> Result<Value<'gc>, Error> {
        let Value::Object(target) = this else {
            return Err(a.throw(ErrorKind::Type, "Bind must be called on a function"));
        };
        if !a.is_callable(this) {
            return Err(a.throw(ErrorKind::Type, "Bind must be called on a function"));
        }
        let pre = args.get(1..).unwrap_or(&[]).to_vec();
        let proto = a.get_prototype_of(target)?;
        let bound = Bound { target: this, this: args.at(0), args: pre.clone() };
        let f = ObjectData { kind: ObjectKind::Bound(bound), proto, ..ObjectData::default() }.alloc(a.mc);
        let mut length = 0.0;
        if a.has_own_property(target, Key::Str("length"))?
            && let Value::Number(n) = a.get_named(this, "length")?
        {
            {
                let n = if n == f64::INFINITY { n } else if n == f64::NEG_INFINITY { 0.0 } else { a.to_integer_or_infinity(n.into())? };
                #[expect(clippy::cast_precision_loss, reason = "实参个数远小于 2^53")]
                let k = pre.len() as f64;
                length = (n - k).max(0.0);
            }
        }
        let name = match a.get_named(this, "name")? {
            Value::String(s) => alloc::format!("bound {}", &**s),
            _ => alloc::string::String::from("bound "),
        };
        let prop = |v: Value<'gc>| Property { slot: Slot::Data(v), enumerable: false, writable: false, configurable: true };
        let n = a.string(&name);
        let mut d = f.borrow_mut(a.mc);
        d.define(a.mc, "length", prop(length.into()));
        d.define(a.mc, "name", prop(n.into()));
        drop(d);
        Ok(f.into())
    }

    fn function_to_string<'gc>(a: &mut Agent<'gc, '_>, this: Value<'gc>, _: &[Value<'gc>]) -> Result<Value<'gc>, Error> {
        if !a.is_callable(this) {
            return Err(a.throw(ErrorKind::Type, "Function.prototype.toString requires that 'this' be a Function"));
        }
        // NativeFunction 语法里名字只能是属性名（可带 get / set）：私有名、`bound f` 这类就不写
        let name = match this {
            Value::Object(o) => match o.borrow().get_own("name").and_then(|p| p.value()) {
                Some(Value::String(s)) if Self::native_name(&s) => alloc::string::String::from(&**s),
                _ => alloc::string::String::new(),
            },
            _ => alloc::string::String::new(),
        };
        Ok(a.string(&alloc::format!("function {name}() {{ [native code] }}")).into())
    }

    /// 能原样写进 `function 名字() { [native code] }` 的名字。
    fn native_name(s: &str) -> bool {
        let bare = s.strip_prefix("get ").or_else(|| s.strip_prefix("set ")).unwrap_or(s);
        let ident = bare.chars().next().is_some_and(|c| c.is_alphabetic() || c == '_' || c == '$') && bare.chars().all(|c| c.is_alphanumeric() || c == '_' || c == '$');
        ident || (bare.starts_with('[') && bare.ends_with(']'))
    }

    /// `Function.prototype` 本身：调用给 `undefined`。
    fn noop<'gc>(_: &mut Agent<'gc, '_>, _: Value<'gc>, _: &[Value<'gc>]) -> Result<Value<'gc>, Error> {
        Ok(Value::Undefined)
    }

    /// `Function.prototype[Symbol.hasInstance](v)`：OrdinaryHasInstance。
    fn has_instance<'gc>(a: &mut Agent<'gc, '_>, this: Value<'gc>, args: &[Value<'gc>]) -> Result<Value<'gc>, Error> {
        Ok(a.ordinary_has_instance(this, args.at(0))?.into())
    }

    fn boolean<'gc>(a: &mut Agent<'gc, '_>, _: Value<'gc>, args: &[Value<'gc>]) -> Result<Value<'gc>, Error> {
        let b = a.to_boolean(args.at(0));
        a.wrap_primitive(b.into(), a.realm.boolean_proto)
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
        let ctor = self.constructor("Object", Objects::object, proto, 1);
        let statics: [(&str, r8frame::NativeFn, u32); 23] = [
            ("assign", Objects::assign, 2),
            ("create", Objects::create, 2),
            ("defineProperties", Objects::define_properties, 2),
            ("defineProperty", Objects::define_property, 3),
            ("entries", Objects::entries, 1),
            ("freeze", Objects::freeze, 1),
            ("fromEntries", Objects::from_entries, 1),
            ("getOwnPropertyDescriptor", Objects::get_own_property_descriptor, 2),
            ("getOwnPropertyDescriptors", Objects::get_own_property_descriptors, 1),
            ("getOwnPropertyNames", Objects::get_own_property_names, 1),
            ("getOwnPropertySymbols", Objects::get_own_property_symbols, 1),
            ("getPrototypeOf", Objects::get_prototype_of, 1),
            ("groupBy", Objects::group_by, 2),
            ("hasOwn", Objects::has_own, 2),
            ("is", Objects::is, 2),
            ("isExtensible", Objects::is_extensible, 1),
            ("isFrozen", Objects::is_frozen, 1),
            ("isSealed", Objects::is_sealed, 1),
            ("keys", Objects::keys, 1),
            ("preventExtensions", Objects::prevent_extensions, 1),
            ("seal", Objects::seal, 1),
            ("setPrototypeOf", Objects::set_prototype_of, 2),
            ("values", Objects::values, 1),
        ];
        for (name, f, n) in statics {
            self.method(ctor, name, f, n);
        }
        let methods: [(&str, r8frame::NativeFn, u32); 10] = [
            ("hasOwnProperty", Objects::has_own_property, 1),
            ("isPrototypeOf", Objects::is_prototype_of, 1),
            ("propertyIsEnumerable", Objects::property_is_enumerable, 1),
            ("toLocaleString", Objects::to_locale_string, 0),
            ("toString", Objects::to_string, 0),
            ("valueOf", Objects::value_of, 0),
            ("__defineGetter__", Objects::define_getter, 2),
            ("__defineSetter__", Objects::define_setter, 2),
            ("__lookupGetter__", Objects::lookup_getter, 1),
            ("__lookupSetter__", Objects::lookup_setter, 1),
        ];
        for (name, f, n) in methods {
            self.method(proto, name, f, n);
        }
        // `__proto__` 是访问器
        let get = self.realm.named_native(self.mc, Objects::proto_getter, "get __proto__", 0);
        let set = self.realm.named_native(self.mc, Objects::proto_setter, "set __proto__", 1);
        proto.borrow_mut(self.mc).define(self.mc, "__proto__", Property { slot: Slot::Accessor { get: Some(get), set: Some(set) }, enumerable: false, writable: false, configurable: true });
    }

    pub(crate) fn function(&mut self) {
        let proto = self.realm.function_proto;
        proto.borrow_mut(self.mc).kind = ObjectKind::Native(r8frame::Native::function(Objects::noop));
        self.realm.name_function(self.mc, proto, "", Some(0));
        self.method(proto, "call", Objects::call, 1);
        self.method(proto, "apply", Objects::apply, 2);
        self.method(proto, "bind", Objects::bind, 1);
        self.method(proto, "toString", Objects::function_to_string, 0);
        let has_instance = self.realm.named_native(self.mc, Objects::has_instance, "[Symbol.hasInstance]", 1);
        proto.borrow_mut(self.mc).define_symbol(self.realm.symbols.has_instance, Property { slot: Slot::Data(has_instance.into()), enumerable: false, writable: false, configurable: false });
        let b = self.realm.boolean_proto;
        // Boolean.prototype 本身是包着 false 的 Boolean 对象
        b.borrow_mut(self.mc).kind = ObjectKind::Primitive(false.into());
        self.constructor("Boolean", Objects::boolean, b, 1);
        self.method(b, "toString", Objects::boolean_to_string, 0);
        self.method(b, "valueOf", Objects::boolean_value_of, 0);
    }
}
