use j8framework::ValueKind;
use alloc::boxed::Box;
use j8framework::{Scope, Collection, Error, ErrorKind, IterKind, JsObject, ObjectData, ObjectKind, Value};

use crate::{Args, Installer};

struct Collections;

struct SetRecord<'scope> {
    set: Value<'scope>,
    size: f64,
    has: Value<'scope>,
    keys: Value<'scope>,
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Shape {
    Map,
    Set,
}

impl Collections {
    fn make<'scope>(a: &mut Scope<'scope, '_>, new_target: Value<'scope>, shape: Shape, weak: bool) -> Result<JsObject<'scope>, Error> {
        let (kind, proto, name) = match (shape, weak) {
            (Shape::Map, false) => (ObjectKind::Map(Box::new(Collection::new(false))), a.context.map_proto, "Map"),
            (Shape::Map, true) => (ObjectKind::Map(Box::new(Collection::new(true))), a.context.weakmap_proto, "WeakMap"),
            (Shape::Set, false) => (ObjectKind::Set(Box::new(Collection::new(false))), a.context.set_proto, "Set"),
            (Shape::Set, true) => (ObjectKind::Set(Box::new(Collection::new(true))), a.context.weakset_proto, "WeakSet"),
        };
        if new_target == Value::Undefined {
            return Err(a.throw(ErrorKind::Type, &alloc::format!("Constructor {name} requires 'new'")));
        }
        a.create_from_ctor(new_target, proto, kind)
    }

    fn this<'scope>(a: &Scope<'scope, '_>, this: Value<'scope>, shape: Shape, weak: bool) -> Result<JsObject<'scope>, Error> {
        let ok = match this.kind() {
            ValueKind::Object(o) => match &o.borrow().kind {
                ObjectKind::Map(c) => shape == Shape::Map && c.weak == weak,
                ObjectKind::Set(c) => shape == Shape::Set && c.weak == weak,
                _ => false,
            },
            _ => false,
        };
        match (ok, this.kind()) {
            (true, ValueKind::Object(o)) => Ok(o),
            _ => Err(a.throw(ErrorKind::Type, "Method called on an incompatible receiver")),
        }
    }

    fn read<'scope, R>(a: &Scope<'scope, '_>, this: Value<'scope>, shape: Shape, f: impl FnOnce(&Collection<'scope>) -> R) -> Result<R, Error> {
        if let Some(o) = this.as_object() {
            match (&o.borrow().kind, shape) {
                (ObjectKind::Map(c), Shape::Map) | (ObjectKind::Set(c), Shape::Set) if !c.weak => return Ok(f(c)),
                _ => {}
            }
        }
        Err(a.throw(ErrorKind::Type, "Method called on an incompatible receiver"))
    }

    fn with<'scope, R>(a: &Scope<'scope, '_>, o: JsObject<'scope>, f: impl FnOnce(&mut Collection<'scope>) -> R) -> R {
        match &mut o.borrow_mut(a.mc).kind {
            ObjectKind::Map(c) | ObjectKind::Set(c) => f(c),
            _ => unreachable!("checked to be a collection"),
        }
    }

    fn weak_key<'scope>(a: &Scope<'scope, '_>, key: Value<'scope>, what: &str) -> Result<(), Error> {
        if a.can_be_held_weakly(key) {
            Ok(())
        } else {
            Err(a.throw(ErrorKind::Type, &alloc::format!("Invalid value used {what}")))
        }
    }

    fn construct<'scope>(a: &mut Scope<'scope, '_>, args: &[Value<'scope>], shape: Shape, weak: bool) -> Result<Value<'scope>, Error> {
        let o = Self::make(a, a.new_target, shape, weak)?;
        let source = args.at(0);
        if source.is_nullish() {
            return Ok(o.into());
        }
        let adder = a.get_named(o.into(), if shape == Shape::Map { "set" } else { "add" })?;
        if !a.is_callable(adder) {
            return Err(a.throw(ErrorKind::Type, "'set' / 'add' of the new collection is not a function"));
        }
        let it = a.get_iterator(source)?;
        while let Some(item) = a.iterator_step(it)? {
            let r = (|| {
                let entry = match shape {
                    Shape::Set => alloc::vec![item],
                    Shape::Map => {
                        if item.as_object().is_none() {
                            let what = a.describe(item);
                            return Err(a.throw(ErrorKind::Type, &alloc::format!("Iterator value {what} is not an entry object")));
                        }
                        alloc::vec![a.get_index(item, 0.0)?, a.get_index(item, 1.0)?]
                    }
                };
                a.call(adder, o.into(), &entry)
            })();
            if let Err(e) = r {
                return Err(a.iterator_close_abrupt(it.iterator, e));
            }
        }
        Ok(o.into())
    }

    fn map<'scope>(a: &mut Scope<'scope, '_>, _: Value<'scope>, args: &[Value<'scope>]) -> Result<Value<'scope>, Error> {
        Self::construct(a, args, Shape::Map, false)
    }

    fn set_ctor<'scope>(a: &mut Scope<'scope, '_>, _: Value<'scope>, args: &[Value<'scope>]) -> Result<Value<'scope>, Error> {
        Self::construct(a, args, Shape::Set, false)
    }

    fn weak_map<'scope>(a: &mut Scope<'scope, '_>, _: Value<'scope>, args: &[Value<'scope>]) -> Result<Value<'scope>, Error> {
        Self::construct(a, args, Shape::Map, true)
    }

    fn weak_set<'scope>(a: &mut Scope<'scope, '_>, _: Value<'scope>, args: &[Value<'scope>]) -> Result<Value<'scope>, Error> {
        Self::construct(a, args, Shape::Set, true)
    }

    fn read_collection<'scope>(this: Value<'scope>, set: bool, f: impl FnOnce(&Collection<'scope>) -> Value<'scope>) -> Option<Value<'scope>> {
        let o = this.as_object()?;
        let r = o.try_borrow()?;
        match &r.kind {
            ObjectKind::Map(c) if !set && !c.weak => Some(f(c)),
            ObjectKind::Set(c) if set && !c.weak => Some(f(c)),
            _ => None,
        }
    }

    fn write_collection<'scope>(a: &Scope<'scope, '_>, this: Value<'scope>, set: bool, f: impl FnOnce(&mut Collection<'scope>)) -> Option<Value<'scope>> {
        let o = this.as_object()?;
        {
            let r = o.try_borrow()?;
            let ok = match &r.kind {
                ObjectKind::Map(c) if !set => !c.weak,
                ObjectKind::Set(c) if set => !c.weak,
                _ => false,
            };
            if !ok {
                return None;
            }
        }
        match &mut o.borrow_mut(a.mc).kind {
            ObjectKind::Map(c) if !set && !c.weak => {
                f(c);
                Some(this)
            }
            ObjectKind::Set(c) if set && !c.weak => {
                f(c);
                Some(this)
            }
            _ => None,
        }
    }

    fn map_get1<'scope>(_: &mut Scope<'scope, '_>, this: Value<'scope>, key: Value<'scope>) -> Option<Value<'scope>> {
        Self::read_collection(this, false, |c| c.get(key).unwrap_or(Value::Undefined))
    }

    fn map_has1<'scope>(_: &mut Scope<'scope, '_>, this: Value<'scope>, key: Value<'scope>) -> Option<Value<'scope>> {
        Self::read_collection(this, false, |c| c.has(key).into())
    }

    fn map_set2<'scope>(a: &mut Scope<'scope, '_>, this: Value<'scope>, key: Value<'scope>, value: Value<'scope>) -> Option<Value<'scope>> {
        Self::write_collection(a, this, false, |c| c.set(key, value))
    }

    fn set_has1<'scope>(_: &mut Scope<'scope, '_>, this: Value<'scope>, key: Value<'scope>) -> Option<Value<'scope>> {
        Self::read_collection(this, true, |c| c.has(key).into())
    }

    fn set_add1<'scope>(a: &mut Scope<'scope, '_>, this: Value<'scope>, key: Value<'scope>) -> Option<Value<'scope>> {
        Self::write_collection(a, this, true, |c| c.set(key, key))
    }

    fn map_get<'scope>(a: &mut Scope<'scope, '_>, this: Value<'scope>, args: &[Value<'scope>]) -> Result<Value<'scope>, Error> {
        Ok(Self::read(a, this, Shape::Map, |c| c.get(args.at(0)))?.unwrap_or_default())
    }

    fn map_set<'scope>(a: &mut Scope<'scope, '_>, this: Value<'scope>, args: &[Value<'scope>]) -> Result<Value<'scope>, Error> {
        let o = Self::this(a, this, Shape::Map, false)?;
        Self::with(a, o, |c| c.set(args.at(0), args.at(1)));
        Ok(this)
    }

    fn map_has<'scope>(a: &mut Scope<'scope, '_>, this: Value<'scope>, args: &[Value<'scope>]) -> Result<Value<'scope>, Error> {
        Ok(Self::read(a, this, Shape::Map, |c| c.has(args.at(0)))?.into())
    }

    fn map_delete<'scope>(a: &mut Scope<'scope, '_>, this: Value<'scope>, args: &[Value<'scope>]) -> Result<Value<'scope>, Error> {
        let o = Self::this(a, this, Shape::Map, false)?;
        Ok(Self::with(a, o, |c| c.delete(args.at(0))).into())
    }

    fn set_add<'scope>(a: &mut Scope<'scope, '_>, this: Value<'scope>, args: &[Value<'scope>]) -> Result<Value<'scope>, Error> {
        let o = Self::this(a, this, Shape::Set, false)?;
        let v = args.at(0);
        Self::with(a, o, |c| c.set(v, v));
        Ok(this)
    }

    fn set_has<'scope>(a: &mut Scope<'scope, '_>, this: Value<'scope>, args: &[Value<'scope>]) -> Result<Value<'scope>, Error> {
        Ok(Self::read(a, this, Shape::Set, |c| c.has(args.at(0)))?.into())
    }

    fn set_delete<'scope>(a: &mut Scope<'scope, '_>, this: Value<'scope>, args: &[Value<'scope>]) -> Result<Value<'scope>, Error> {
        let o = Self::this(a, this, Shape::Set, false)?;
        Ok(Self::with(a, o, |c| c.delete(args.at(0))).into())
    }

    fn shape(set: bool) -> Shape {
        if set { Shape::Set } else { Shape::Map }
    }

    fn clear<'scope, const SET: bool>(a: &mut Scope<'scope, '_>, this: Value<'scope>, _: &[Value<'scope>]) -> Result<Value<'scope>, Error> {
        let o = Self::this(a, this, Self::shape(SET), false)?;
        Self::with(a, o, Collection::clear);
        Ok(Value::Undefined)
    }

    fn size<'scope, const SET: bool>(a: &mut Scope<'scope, '_>, this: Value<'scope>, _: &[Value<'scope>]) -> Result<Value<'scope>, Error> {
        let o = Self::this(a, this, Self::shape(SET), false)?;
        Ok(Self::size_of(a, o).into())
    }

    fn size_of<'scope>(a: &Scope<'scope, '_>, o: JsObject<'scope>) -> f64 {
        f64::from(u32::try_from(Self::with(a, o, |c| c.size)).unwrap_or(u32::MAX))
    }

    fn entry_from<'scope>(a: &Scope<'scope, '_>, o: JsObject<'scope>, i: usize) -> Option<(usize, Value<'scope>, Value<'scope>)> {
        Self::with(a, o, |c| c.entries().iter().enumerate().skip(i).find_map(|(j, e)| e.map(|(k, v)| (j, k, v))))
    }

    fn for_each<'scope, const SET: bool>(a: &mut Scope<'scope, '_>, this: Value<'scope>, args: &[Value<'scope>]) -> Result<Value<'scope>, Error> {
        let o = Self::this(a, this, Self::shape(SET), false)?;
        let f = args.at(0);
        if !a.is_callable(f) {
            return Err(a.throw(ErrorKind::Type, "callback is not a function"));
        }
        let cb = a.callback(f, args.at(1));
        let mut i = 0;
        let mut quiet = false;
        while let Some((j, k, v)) = Self::entry_from(a, o, i) {
            let all = [v, k, this];
            let args = &all[..cb.passed().map_or(3, |n| n.min(3))];
            if quiet { cb.again(a, args)? } else { cb.call(a, args)? };
            quiet = true;
            i = j + 1;
        }
        Ok(Value::Undefined)
    }

    fn iterator<'scope>(a: &Scope<'scope, '_>, o: JsObject<'scope>, kind: IterKind, set: bool) -> Value<'scope> {
        let proto = if set { a.context.set_iterator_proto } else { a.context.collection_iterator_proto };
        ObjectData { kind: ObjectKind::CollectionIterator { target: Some(o), index: 0, kind, set }, proto: Some(proto), ..ObjectData::default() }.alloc(a.mc).into()
    }

    fn entries<'scope, const SET: bool>(a: &mut Scope<'scope, '_>, this: Value<'scope>, _: &[Value<'scope>]) -> Result<Value<'scope>, Error> {
        let o = Self::this(a, this, Self::shape(SET), false)?;
        Ok(Self::iterator(a, o, IterKind::Entries, SET))
    }

    fn keys<'scope, const SET: bool>(a: &mut Scope<'scope, '_>, this: Value<'scope>, _: &[Value<'scope>]) -> Result<Value<'scope>, Error> {
        let o = Self::this(a, this, Self::shape(SET), false)?;
        Ok(Self::iterator(a, o, IterKind::Keys, SET))
    }

    fn values<'scope, const SET: bool>(a: &mut Scope<'scope, '_>, this: Value<'scope>, _: &[Value<'scope>]) -> Result<Value<'scope>, Error> {
        let o = Self::this(a, this, Self::shape(SET), false)?;
        Ok(Self::iterator(a, o, IterKind::Values, SET))
    }

    fn next<'scope, const SET: bool>(a: &mut Scope<'scope, '_>, this: Value<'scope>, _: &[Value<'scope>]) -> Result<Value<'scope>, Error> {
        let state = match this.kind() {
            ValueKind::Object(it) => match it.borrow().kind {
                ObjectKind::CollectionIterator { target, index, kind, set } if set == SET => Some((it, target, index, kind)),
                _ => None,
            },
            _ => None,
        };
        let Some((it, target, index, kind)) = state else {
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

    fn group_by<'scope>(a: &mut Scope<'scope, '_>, _: Value<'scope>, args: &[Value<'scope>]) -> Result<Value<'scope>, Error> {
        let groups = crate::object::Objects::group(a, args.at(0), args.at(1), false)?;
        let m = ObjectData { kind: ObjectKind::Map(Box::new(Collection::new(false))), proto: Some(a.context.map_proto), ..ObjectData::default() }.alloc(a.mc);
        for (k, items) in groups {
            let arr = a.array(items);
            Self::with(a, m, |c| c.set(k, arr.into()));
        }
        Ok(m.into())
    }

    fn set_record<'scope>(a: &mut Scope<'scope, '_>, other: Value<'scope>) -> Result<SetRecord<'scope>, Error> {
        if other.as_object().is_none() {
            return Err(a.throw(ErrorKind::Type, "The argument must be a set-like object"));
        }
        let raw = a.get_named(other, "size")?;
        let n = a.to_number(raw)?;
        if n.is_nan() {
            return Err(a.throw(ErrorKind::Type, "The 'size' of a set-like object must be a number"));
        }
        let size = a.to_integer_or_infinity(n.into())?;
        if size < 0.0 {
            return Err(a.throw(ErrorKind::Range, "The 'size' of a set-like object must not be negative"));
        }
        let has = a.get_named(other, "has")?;
        if !a.is_callable(has) {
            return Err(a.throw(ErrorKind::Type, "The 'has' of a set-like object must be a function"));
        }
        let keys = a.get_named(other, "keys")?;
        if !a.is_callable(keys) {
            return Err(a.throw(ErrorKind::Type, "The 'keys' of a set-like object must be a function"));
        }
        Ok(SetRecord { set: other, size, has, keys })
    }

    fn keys_iterator<'scope>(a: &mut Scope<'scope, '_>, r: &SetRecord<'scope>) -> Result<j8framework::IterRecord<'scope>, Error> {
        let iterator = a.call(r.keys, r.set, &[])?;
        if iterator.as_object().is_none() {
            return Err(a.throw(ErrorKind::Type, "keys() of a set-like object must return an object"));
        }
        let next = a.get_named(iterator, "next")?;
        Ok(j8framework::IterRecord { iterator, next })
    }

    fn canonical(v: Value<'_>) -> Value<'_> {
        match v.kind() {
            ValueKind::Number(0.0) => Value::Number(0.0),
            v => Value::from(v),
        }
    }

    fn new_set<'scope>(a: &Scope<'scope, '_>, items: impl IntoIterator<Item = Value<'scope>>) -> Value<'scope> {
        let mut c = Collection::new(false);
        for v in items {
            c.set(v, v);
        }
        ObjectData { kind: ObjectKind::Set(Box::new(c)), proto: Some(a.context.set_proto), ..ObjectData::default() }.alloc(a.mc).into()
    }

    fn set_values<'scope>(a: &Scope<'scope, '_>, o: JsObject<'scope>) -> alloc::vec::Vec<Value<'scope>> {
        Self::with(a, o, |c| c.entries().iter().flatten().map(|&(k, _)| k).collect())
    }

    fn union<'scope>(a: &mut Scope<'scope, '_>, this: Value<'scope>, args: &[Value<'scope>]) -> Result<Value<'scope>, Error> {
        let o = Self::this(a, this, Shape::Set, false)?;
        let r = Self::set_record(a, args.at(0))?;
        let it = Self::keys_iterator(a, &r)?;
        let mut out = Self::set_values(a, o);
        while let Some(v) = a.iterator_step(it)? {
            let v = Self::canonical(v);
            if !out.iter().any(|x| Self::same(*x, v)) {
                out.push(v);
            }
        }
        Ok(Self::new_set(a, out))
    }

    fn same<'scope>(x: Value<'scope>, y: Value<'scope>) -> bool {
        x == y || matches!((x.kind(), y.kind()), (ValueKind::Number(p), ValueKind::Number(q)) if p.is_nan() && q.is_nan())
    }

    fn intersection<'scope>(a: &mut Scope<'scope, '_>, this: Value<'scope>, args: &[Value<'scope>]) -> Result<Value<'scope>, Error> {
        let o = Self::this(a, this, Shape::Set, false)?;
        let r = Self::set_record(a, args.at(0))?;
        let mut out: alloc::vec::Vec<Value<'scope>> = alloc::vec::Vec::new();
        if Self::size_of(a, o) <= r.size {
            let mut i = 0;
            while let Some((j, e, _)) = Self::entry_from(a, o, i) {
                let hit = a.call(r.has, r.set, &[e])?;
                if a.to_boolean(hit) && !out.iter().any(|x| Self::same(*x, e)) {
                    out.push(e);
                }
                i = j + 1;
            }
        } else {
            let it = Self::keys_iterator(a, &r)?;
            while let Some(v) = a.iterator_step(it)? {
                let v = Self::canonical(v);
                if Self::with(a, o, |c| c.has(v)) && !out.iter().any(|x| Self::same(*x, v)) {
                    out.push(v);
                }
            }
        }
        Ok(Self::new_set(a, out))
    }

    fn difference<'scope>(a: &mut Scope<'scope, '_>, this: Value<'scope>, args: &[Value<'scope>]) -> Result<Value<'scope>, Error> {
        let o = Self::this(a, this, Shape::Set, false)?;
        let r = Self::set_record(a, args.at(0))?;
        let mut out = Self::set_values(a, o);
        if Self::size_of(a, o) <= r.size {
            let mut keep = alloc::vec::Vec::new();
            for e in out {
                let hit = a.call(r.has, r.set, &[e])?;
                if !a.to_boolean(hit) {
                    keep.push(e);
                }
            }
            out = keep;
        } else {
            let it = Self::keys_iterator(a, &r)?;
            while let Some(v) = a.iterator_step(it)? {
                let v = Self::canonical(v);
                out.retain(|x| !Self::same(*x, v));
            }
        }
        Ok(Self::new_set(a, out))
    }

    fn symmetric_difference<'scope>(a: &mut Scope<'scope, '_>, this: Value<'scope>, args: &[Value<'scope>]) -> Result<Value<'scope>, Error> {
        let o = Self::this(a, this, Shape::Set, false)?;
        let r = Self::set_record(a, args.at(0))?;
        let it = Self::keys_iterator(a, &r)?;
        let mut out = Self::set_values(a, o);
        while let Some(v) = a.iterator_step(it)? {
            let v = Self::canonical(v);
            let in_result = out.iter().position(|x| Self::same(*x, v));
            if Self::with(a, o, |c| c.has(v)) {
                if let Some(i) = in_result {
                    out.remove(i);
                }
            } else if in_result.is_none() {
                out.push(v);
            }
        }
        Ok(Self::new_set(a, out))
    }

    fn is_subset_of<'scope>(a: &mut Scope<'scope, '_>, this: Value<'scope>, args: &[Value<'scope>]) -> Result<Value<'scope>, Error> {
        let o = Self::this(a, this, Shape::Set, false)?;
        let r = Self::set_record(a, args.at(0))?;
        if Self::size_of(a, o) > r.size {
            return Ok(false.into());
        }
        let mut i = 0;
        while let Some((j, e, _)) = Self::entry_from(a, o, i) {
            let hit = a.call(r.has, r.set, &[e])?;
            if !a.to_boolean(hit) {
                return Ok(false.into());
            }
            i = j + 1;
        }
        Ok(true.into())
    }

    fn is_superset_of<'scope>(a: &mut Scope<'scope, '_>, this: Value<'scope>, args: &[Value<'scope>]) -> Result<Value<'scope>, Error> {
        let o = Self::this(a, this, Shape::Set, false)?;
        let r = Self::set_record(a, args.at(0))?;
        if Self::size_of(a, o) < r.size {
            return Ok(false.into());
        }
        let it = Self::keys_iterator(a, &r)?;
        while let Some(v) = a.iterator_step(it)? {
            if !Self::with(a, o, |c| c.has(v)) {
                a.iterator_close(it.iterator)?;
                return Ok(false.into());
            }
        }
        Ok(true.into())
    }

    fn is_disjoint_from<'scope>(a: &mut Scope<'scope, '_>, this: Value<'scope>, args: &[Value<'scope>]) -> Result<Value<'scope>, Error> {
        let o = Self::this(a, this, Shape::Set, false)?;
        let r = Self::set_record(a, args.at(0))?;
        if Self::size_of(a, o) <= r.size {
            let mut i = 0;
            while let Some((j, e, _)) = Self::entry_from(a, o, i) {
                let hit = a.call(r.has, r.set, &[e])?;
                if a.to_boolean(hit) {
                    return Ok(false.into());
                }
                i = j + 1;
            }
        } else {
            let it = Self::keys_iterator(a, &r)?;
            while let Some(v) = a.iterator_step(it)? {
                if Self::with(a, o, |c| c.has(v)) {
                    a.iterator_close(it.iterator)?;
                    return Ok(false.into());
                }
            }
        }
        Ok(true.into())
    }

    fn weak_get<'scope>(a: &mut Scope<'scope, '_>, this: Value<'scope>, args: &[Value<'scope>]) -> Result<Value<'scope>, Error> {
        let o = Self::this(a, this, Shape::Map, true)?;
        Ok(Self::with(a, o, |c| c.get(args.at(0))).unwrap_or_default())
    }

    fn weak_set_entry<'scope>(a: &mut Scope<'scope, '_>, this: Value<'scope>, args: &[Value<'scope>]) -> Result<Value<'scope>, Error> {
        let o = Self::this(a, this, Shape::Map, true)?;
        Self::weak_key(a, args.at(0), "as weak map key")?;
        Self::with(a, o, |c| c.set(args.at(0), args.at(1)));
        Ok(this)
    }

    fn weak_map_has<'scope>(a: &mut Scope<'scope, '_>, this: Value<'scope>, args: &[Value<'scope>]) -> Result<Value<'scope>, Error> {
        let o = Self::this(a, this, Shape::Map, true)?;
        Ok(Self::with(a, o, |c| c.has(args.at(0))).into())
    }

    fn weak_map_delete<'scope>(a: &mut Scope<'scope, '_>, this: Value<'scope>, args: &[Value<'scope>]) -> Result<Value<'scope>, Error> {
        let o = Self::this(a, this, Shape::Map, true)?;
        Ok(Self::with(a, o, |c| c.delete(args.at(0))).into())
    }

    fn weak_add<'scope>(a: &mut Scope<'scope, '_>, this: Value<'scope>, args: &[Value<'scope>]) -> Result<Value<'scope>, Error> {
        let o = Self::this(a, this, Shape::Set, true)?;
        let v = args.at(0);
        Self::weak_key(a, v, "in weak set")?;
        Self::with(a, o, |c| c.set(v, v));
        Ok(this)
    }

    fn weak_set_has<'scope>(a: &mut Scope<'scope, '_>, this: Value<'scope>, args: &[Value<'scope>]) -> Result<Value<'scope>, Error> {
        let o = Self::this(a, this, Shape::Set, true)?;
        Ok(Self::with(a, o, |c| c.has(args.at(0))).into())
    }

    fn weak_set_delete<'scope>(a: &mut Scope<'scope, '_>, this: Value<'scope>, args: &[Value<'scope>]) -> Result<Value<'scope>, Error> {
        let o = Self::this(a, this, Shape::Set, true)?;
        Ok(Self::with(a, o, |c| c.delete(args.at(0))).into())
    }
}

impl Installer<'_, '_> {
    pub(crate) fn collections(&mut self) {
        let r = self.context;
        let iter = r.symbols.iterator;
        let map = r.map_proto;
        let map_ctor = self.constructor("Map", Collections::map, map, 0);
        self.method(map_ctor, "groupBy", Collections::group_by, 2);
        let methods: [(&str, j8framework::NativeFn, u32); 8] = [
            ("get", Collections::map_get, 1),
            ("set", Collections::map_set, 2),
            ("has", Collections::map_has, 1),
            ("delete", Collections::map_delete, 1),
            ("clear", Collections::clear::<false>, 0),
            ("forEach", Collections::for_each::<false>, 1),
            ("keys", Collections::keys::<false>, 0),
            ("values", Collections::values::<false>, 0),
        ];
        for (name, f, n) in methods {
            self.method(map, name, f, n);
        }
        self.direct(Collections::map_get, j8framework::FastCall::A1(Collections::map_get1), j8framework::FastOut::Value);
        self.direct(Collections::map_has, j8framework::FastCall::A1(Collections::map_has1), j8framework::FastOut::Bool);
        self.direct(Collections::map_set, j8framework::FastCall::A2(Collections::map_set2), j8framework::FastOut::Value);
        let entries = r.named_native(self.mc, Collections::entries::<false>, "entries", 0);
        self.set(map, "entries", entries.into());
        map.borrow_mut(self.mc).define_symbol(iter, j8framework::Property::hidden(entries.into()));
        self.getter(map, "size", Collections::size::<false>);
        let set = r.set_proto;
        self.constructor("Set", Collections::set_ctor, set, 0);
        let methods: [(&str, j8framework::NativeFn, u32); 13] = [
            ("add", Collections::set_add, 1),
            ("has", Collections::set_has, 1),
            ("delete", Collections::set_delete, 1),
            ("clear", Collections::clear::<true>, 0),
            ("forEach", Collections::for_each::<true>, 1),
            ("entries", Collections::entries::<true>, 0),
            ("union", Collections::union, 1),
            ("intersection", Collections::intersection, 1),
            ("difference", Collections::difference, 1),
            ("symmetricDifference", Collections::symmetric_difference, 1),
            ("isSubsetOf", Collections::is_subset_of, 1),
            ("isSupersetOf", Collections::is_superset_of, 1),
            ("isDisjointFrom", Collections::is_disjoint_from, 1),
        ];
        for (name, f, n) in methods {
            self.method(set, name, f, n);
        }
        self.direct(Collections::set_has, j8framework::FastCall::A1(Collections::set_has1), j8framework::FastOut::Bool);
        self.direct(Collections::set_add, j8framework::FastCall::A1(Collections::set_add1), j8framework::FastOut::Value);
        let values = r.named_native(self.mc, Collections::values::<true>, "values", 0);
        self.set(set, "values", values.into());
        self.set(set, "keys", values.into());
        set.borrow_mut(self.mc).define_symbol(iter, j8framework::Property::hidden(values.into()));
        self.getter(set, "size", Collections::size::<true>);
        self.method(r.collection_iterator_proto, "next", Collections::next::<false>, 0);
        self.method(r.set_iterator_proto, "next", Collections::next::<true>, 0);
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
