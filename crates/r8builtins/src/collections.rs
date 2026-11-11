use r8frame::{Agent, Collection, Error, ErrorKind, IterKind, JsObject, ObjectData, ObjectKind, Value};

use crate::{Args, Installer};

/// Map / Set / WeakMap / WeakSet。弱集合目前强引用键（还没有 ephemeron），只多了「键必须是对象」的检查。
struct Collections;

/// 集合的种类。
#[derive(Clone, Copy, PartialEq, Eq)]
enum Shape {
    Map,
    Set,
}

impl Collections {
    fn make<'gc>(a: &mut Agent<'gc, '_>, shape: Shape, weak: bool) -> Result<JsObject<'gc>, Error> {
        let (kind, proto, name) = match (shape, weak) {
            (Shape::Map, false) => (ObjectKind::Map(Collection::new(false)), a.realm.map_proto, "Map"),
            (Shape::Map, true) => (ObjectKind::Map(Collection::new(true)), a.realm.weakmap_proto, "WeakMap"),
            (Shape::Set, false) => (ObjectKind::Set(Collection::new(false)), a.realm.set_proto, "Set"),
            (Shape::Set, true) => (ObjectKind::Set(Collection::new(true)), a.realm.weakset_proto, "WeakSet"),
        };
        if a.new_target == Value::Undefined {
            return Err(a.throw(ErrorKind::Type, &alloc::format!("Constructor {name} requires 'new'")));
        }
        a.create_from_ctor(proto, kind)
    }

    /// `this` 是不是这种集合（弱 / 非弱也要对得上）。
    fn this<'gc>(a: &Agent<'gc, '_>, this: Value<'gc>, shape: Shape, weak: bool) -> Result<JsObject<'gc>, Error> {
        let ok = match this {
            Value::Object(o) => match &o.borrow().kind {
                ObjectKind::Map(c) => shape == Shape::Map && c.weak == weak,
                ObjectKind::Set(c) => shape == Shape::Set && c.weak == weak,
                _ => false,
            },
            _ => false,
        };
        match (ok, this) {
            (true, Value::Object(o)) => Ok(o),
            _ => Err(a.throw(ErrorKind::Type, "Method called on an incompatible receiver")),
        }
    }

    /// 对集合本体做事。
    fn with<'gc, R>(a: &Agent<'gc, '_>, o: JsObject<'gc>, f: impl FnOnce(&mut Collection<'gc>) -> R) -> R {
        match &mut o.borrow_mut(a.mc).kind {
            ObjectKind::Map(c) | ObjectKind::Set(c) => f(c),
            _ => unreachable!("checked to be a collection"),
        }
    }

    /// 弱集合的键必须是对象（或 Symbol）。
    fn weak_key<'gc>(a: &Agent<'gc, '_>, key: Value<'gc>, what: &str) -> Result<(), Error> {
        if matches!(key, Value::Object(_) | Value::Symbol(_)) {
            Ok(())
        } else {
            Err(a.throw(ErrorKind::Type, &alloc::format!("Invalid value used {what}")))
        }
    }

    /// 构造：可选的可迭代对象逐项放进去（Map 的每项要是 `[键, 值]` 对象）。
    fn construct<'gc>(a: &mut Agent<'gc, '_>, args: &[Value<'gc>], shape: Shape, weak: bool) -> Result<Value<'gc>, Error> {
        let o = Self::make(a, shape, weak)?;
        let source = args.at(0);
        if source.is_nullish() {
            return Ok(o.into());
        }
        let it = a.get_iterator(source)?;
        while let Some(item) = a.iterator_step(it)? {
            let (k, v) = match shape {
                Shape::Set => (item, item),
                Shape::Map => {
                    if !matches!(item, Value::Object(_)) {
                        a.iterator_close(it.iterator)?;
                        let what = a.to_string(item)?;
                        return Err(a.throw(ErrorKind::Type, &alloc::format!("Iterator value {} is not an entry object", &**what)));
                    }
                    (a.get_keyed(item, 0.0.into())?, a.get_keyed(item, 1.0.into())?)
                }
            };
            if weak {
                Self::weak_key(a, k, if shape == Shape::Map { "as weak map key" } else { "in weak set" })?;
            }
            Self::with(a, o, |c| c.set(k, v));
        }
        Ok(o.into())
    }

    fn map<'gc>(a: &mut Agent<'gc, '_>, _: Value<'gc>, args: &[Value<'gc>]) -> Result<Value<'gc>, Error> {
        Self::construct(a, args, Shape::Map, false)
    }

    fn set_ctor<'gc>(a: &mut Agent<'gc, '_>, _: Value<'gc>, args: &[Value<'gc>]) -> Result<Value<'gc>, Error> {
        Self::construct(a, args, Shape::Set, false)
    }

    fn weak_map<'gc>(a: &mut Agent<'gc, '_>, _: Value<'gc>, args: &[Value<'gc>]) -> Result<Value<'gc>, Error> {
        Self::construct(a, args, Shape::Map, true)
    }

    fn weak_set<'gc>(a: &mut Agent<'gc, '_>, _: Value<'gc>, args: &[Value<'gc>]) -> Result<Value<'gc>, Error> {
        Self::construct(a, args, Shape::Set, true)
    }

    fn map_get<'gc>(a: &mut Agent<'gc, '_>, this: Value<'gc>, args: &[Value<'gc>]) -> Result<Value<'gc>, Error> {
        let o = Self::this(a, this, Shape::Map, false)?;
        Ok(Self::with(a, o, |c| c.get(args.at(0))).unwrap_or_default())
    }

    fn map_set<'gc>(a: &mut Agent<'gc, '_>, this: Value<'gc>, args: &[Value<'gc>]) -> Result<Value<'gc>, Error> {
        let o = Self::this(a, this, Shape::Map, false)?;
        Self::with(a, o, |c| c.set(args.at(0), args.at(1)));
        Ok(this)
    }

    fn map_has<'gc>(a: &mut Agent<'gc, '_>, this: Value<'gc>, args: &[Value<'gc>]) -> Result<Value<'gc>, Error> {
        let o = Self::this(a, this, Shape::Map, false)?;
        Ok(Self::with(a, o, |c| c.has(args.at(0))).into())
    }

    fn map_delete<'gc>(a: &mut Agent<'gc, '_>, this: Value<'gc>, args: &[Value<'gc>]) -> Result<Value<'gc>, Error> {
        let o = Self::this(a, this, Shape::Map, false)?;
        Ok(Self::with(a, o, |c| c.delete(args.at(0))).into())
    }

    fn set_add<'gc>(a: &mut Agent<'gc, '_>, this: Value<'gc>, args: &[Value<'gc>]) -> Result<Value<'gc>, Error> {
        let o = Self::this(a, this, Shape::Set, false)?;
        let v = args.at(0);
        Self::with(a, o, |c| c.set(v, v));
        Ok(this)
    }

    fn set_has<'gc>(a: &mut Agent<'gc, '_>, this: Value<'gc>, args: &[Value<'gc>]) -> Result<Value<'gc>, Error> {
        let o = Self::this(a, this, Shape::Set, false)?;
        Ok(Self::with(a, o, |c| c.has(args.at(0))).into())
    }

    fn set_delete<'gc>(a: &mut Agent<'gc, '_>, this: Value<'gc>, args: &[Value<'gc>]) -> Result<Value<'gc>, Error> {
        let o = Self::this(a, this, Shape::Set, false)?;
        Ok(Self::with(a, o, |c| c.delete(args.at(0))).into())
    }

    /// Map / Set 共用的：`this` 是两者之一（非弱）。
    fn either<'gc>(a: &Agent<'gc, '_>, this: Value<'gc>) -> Result<JsObject<'gc>, Error> {
        Self::this(a, this, Shape::Map, false).or_else(|_| Self::this(a, this, Shape::Set, false))
    }

    fn clear<'gc>(a: &mut Agent<'gc, '_>, this: Value<'gc>, _: &[Value<'gc>]) -> Result<Value<'gc>, Error> {
        let o = Self::either(a, this)?;
        Self::with(a, o, Collection::clear);
        Ok(Value::Undefined)
    }

    fn size<'gc>(a: &mut Agent<'gc, '_>, this: Value<'gc>, _: &[Value<'gc>]) -> Result<Value<'gc>, Error> {
        let o = Self::either(a, this)?;
        Ok(f64::from(u32::try_from(Self::with(a, o, |c| c.size)).unwrap_or(u32::MAX)).into())
    }

    /// 第 `i` 个及以后第一个还在的条目。
    fn entry_from<'gc>(a: &Agent<'gc, '_>, o: JsObject<'gc>, i: usize) -> Option<(usize, Value<'gc>, Value<'gc>)> {
        Self::with(a, o, |c| c.entries.iter().enumerate().skip(i).find_map(|(j, e)| e.map(|(k, v)| (j, k, v))))
    }

    /// `forEach(cb, thisArg)`：活着走，回调里加的项也会走到。Set 的回调是 `(值, 值, set)`。
    fn for_each<'gc>(a: &mut Agent<'gc, '_>, this: Value<'gc>, args: &[Value<'gc>]) -> Result<Value<'gc>, Error> {
        let o = Self::either(a, this)?;
        let f = args.at(0);
        if !a.is_callable(f) {
            return Err(a.throw(ErrorKind::Type, "callback is not a function"));
        }
        let mut i = 0;
        while let Some((j, k, v)) = Self::entry_from(a, o, i) {
            a.call(f, args.at(1), &[v, k, this])?;
            i = j + 1;
        }
        Ok(Value::Undefined)
    }

    fn iterator<'gc>(a: &Agent<'gc, '_>, o: JsObject<'gc>, kind: IterKind) -> Value<'gc> {
        ObjectData { kind: ObjectKind::CollectionIterator { target: Some(o), index: 0, kind }, proto: Some(a.realm.collection_iterator_proto), ..ObjectData::default() }.alloc(a.mc).into()
    }

    fn entries<'gc>(a: &mut Agent<'gc, '_>, this: Value<'gc>, _: &[Value<'gc>]) -> Result<Value<'gc>, Error> {
        let o = Self::either(a, this)?;
        Ok(Self::iterator(a, o, IterKind::Entries))
    }

    fn keys<'gc>(a: &mut Agent<'gc, '_>, this: Value<'gc>, _: &[Value<'gc>]) -> Result<Value<'gc>, Error> {
        let o = Self::either(a, this)?;
        Ok(Self::iterator(a, o, IterKind::Keys))
    }

    fn values<'gc>(a: &mut Agent<'gc, '_>, this: Value<'gc>, _: &[Value<'gc>]) -> Result<Value<'gc>, Error> {
        let o = Self::either(a, this)?;
        Ok(Self::iterator(a, o, IterKind::Values))
    }

    /// 迭代器的 `next`：从记下的下标往后找下一个还在的条目。
    fn next<'gc>(a: &mut Agent<'gc, '_>, this: Value<'gc>, _: &[Value<'gc>]) -> Result<Value<'gc>, Error> {
        let Value::Object(it) = this else {
            return Err(a.throw(ErrorKind::Type, "next method called on an incompatible receiver"));
        };
        let ObjectKind::CollectionIterator { target, index, kind } = it.borrow().kind else {
            return Err(a.throw(ErrorKind::Type, "next method called on an incompatible receiver"));
        };
        let found = target.and_then(|o| Self::entry_from(a, o, index));
        if let ObjectKind::CollectionIterator { target, index, .. } = &mut it.borrow_mut(a.mc).kind {
            match found {
                Some((j, ..)) => *index = j + 1,
                None => *target = None,
            }
        }
        let Some((_, k, v)) = found else { return Ok(a.iter_result(Value::Undefined, true).into()) };
        let out = match kind {
            IterKind::Keys => k,
            IterKind::Values => v,
            IterKind::Entries => a.array(alloc::vec![k, v]).into(),
        };
        Ok(a.iter_result(out, false).into())
    }

    fn weak_get<'gc>(a: &mut Agent<'gc, '_>, this: Value<'gc>, args: &[Value<'gc>]) -> Result<Value<'gc>, Error> {
        let o = Self::this(a, this, Shape::Map, true)?;
        Ok(Self::with(a, o, |c| c.get(args.at(0))).unwrap_or_default())
    }

    fn weak_set_entry<'gc>(a: &mut Agent<'gc, '_>, this: Value<'gc>, args: &[Value<'gc>]) -> Result<Value<'gc>, Error> {
        let o = Self::this(a, this, Shape::Map, true)?;
        Self::weak_key(a, args.at(0), "as weak map key")?;
        Self::with(a, o, |c| c.set(args.at(0), args.at(1)));
        Ok(this)
    }

    fn weak_map_has<'gc>(a: &mut Agent<'gc, '_>, this: Value<'gc>, args: &[Value<'gc>]) -> Result<Value<'gc>, Error> {
        let o = Self::this(a, this, Shape::Map, true)?;
        Ok(Self::with(a, o, |c| c.has(args.at(0))).into())
    }

    fn weak_map_delete<'gc>(a: &mut Agent<'gc, '_>, this: Value<'gc>, args: &[Value<'gc>]) -> Result<Value<'gc>, Error> {
        let o = Self::this(a, this, Shape::Map, true)?;
        Ok(Self::with(a, o, |c| c.delete(args.at(0))).into())
    }

    fn weak_add<'gc>(a: &mut Agent<'gc, '_>, this: Value<'gc>, args: &[Value<'gc>]) -> Result<Value<'gc>, Error> {
        let o = Self::this(a, this, Shape::Set, true)?;
        let v = args.at(0);
        Self::weak_key(a, v, "in weak set")?;
        Self::with(a, o, |c| c.set(v, v));
        Ok(this)
    }

    fn weak_set_has<'gc>(a: &mut Agent<'gc, '_>, this: Value<'gc>, args: &[Value<'gc>]) -> Result<Value<'gc>, Error> {
        let o = Self::this(a, this, Shape::Set, true)?;
        Ok(Self::with(a, o, |c| c.has(args.at(0))).into())
    }

    fn weak_set_delete<'gc>(a: &mut Agent<'gc, '_>, this: Value<'gc>, args: &[Value<'gc>]) -> Result<Value<'gc>, Error> {
        let o = Self::this(a, this, Shape::Set, true)?;
        Ok(Self::with(a, o, |c| c.delete(args.at(0))).into())
    }
}

impl Installer<'_, '_> {
    pub(crate) fn collections(&mut self) {
        let r = self.realm;
        let iter = r.symbols.iterator;
        // Map：`entries` 与 `[Symbol.iterator]` 是同一个函数
        let map = r.map_proto;
        self.constructor("Map", Collections::map, map, 0);
        let methods: [(&str, r8frame::NativeFn, u32); 7] = [
            ("get", Collections::map_get, 1),
            ("set", Collections::map_set, 2),
            ("has", Collections::map_has, 1),
            ("delete", Collections::map_delete, 1),
            ("clear", Collections::clear, 0),
            ("forEach", Collections::for_each, 1),
            ("keys", Collections::keys, 0),
        ];
        for (name, f, n) in methods {
            self.method(map, name, f, n);
        }
        self.method(map, "values", Collections::values, 0);
        let entries = r.named_native(self.mc, Collections::entries, "entries", 0);
        self.set(map, "entries", entries.into());
        map.borrow_mut(self.mc).define_symbol(iter, r8frame::Property::hidden(entries.into()));
        self.getter(map, "size", Collections::size);
        // Set：`values` / `keys` / `[Symbol.iterator]` 是同一个函数
        let set = r.set_proto;
        self.constructor("Set", Collections::set_ctor, set, 0);
        let methods: [(&str, r8frame::NativeFn, u32); 6] = [
            ("add", Collections::set_add, 1),
            ("has", Collections::set_has, 1),
            ("delete", Collections::set_delete, 1),
            ("clear", Collections::clear, 0),
            ("forEach", Collections::for_each, 1),
            ("entries", Collections::entries, 0),
        ];
        for (name, f, n) in methods {
            self.method(set, name, f, n);
        }
        let values = r.named_native(self.mc, Collections::values, "values", 0);
        self.set(set, "values", values.into());
        self.set(set, "keys", values.into());
        set.borrow_mut(self.mc).define_symbol(iter, r8frame::Property::hidden(values.into()));
        self.getter(set, "size", Collections::size);
        self.method(r.collection_iterator_proto, "next", Collections::next, 0);
        // 弱集合
        let wm = r.weakmap_proto;
        self.constructor("WeakMap", Collections::weak_map, wm, 0);
        self.method(wm, "get", Collections::weak_get, 1);
        self.method(wm, "set", Collections::weak_set_entry, 2);
        self.method(wm, "has", Collections::weak_map_has, 1);
        self.method(wm, "delete", Collections::weak_map_delete, 1);
        let ws = r.weakset_proto;
        self.constructor("WeakSet", Collections::weak_set, ws, 0);
        self.method(ws, "add", Collections::weak_add, 1);
        self.method(ws, "has", Collections::weak_set_has, 1);
        self.method(ws, "delete", Collections::weak_set_delete, 1);
    }
}
