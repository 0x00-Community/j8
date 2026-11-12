use alloc::vec::Vec;

use r8frame::{Agent, Error, ErrorKind, HelperKind, HelperState, IterRecord, IteratorHelper, JsObject, Key, ObjectData, ObjectKind, Property, Slot, Value};

use crate::{Args, Installer};

/// 全局 `Iterator`、`Iterator.prototype` 上的助手方法、`Iterator.from`。
struct Helpers;

/// `some` / `every` / `find` 怎么收。
#[derive(Clone, Copy, PartialEq, Eq)]
enum Probe {
    Some,
    Every,
    Find,
}

impl Helpers {
    fn is_iterator_ctor(v: Value<'_>) -> bool {
        matches!(v, Value::Object(f) if matches!(f.borrow().kind, ObjectKind::Native(n) if core::ptr::fn_addr_eq(n.f, Self::iterator as r8frame::NativeFn)))
    }

    /// `Iterator` 是抽象类：只能被子类构造。
    fn iterator<'gc>(a: &mut Agent<'gc, '_>, _: Value<'gc>, _: &[Value<'gc>]) -> Result<Value<'gc>, Error> {
        if a.new_target == Value::Undefined || Self::is_iterator_ctor(a.new_target) {
            return Err(a.throw(ErrorKind::Type, "Abstract class Iterator not directly constructable"));
        }
        Ok(a.create_from_ctor(a.realm.iterator_proto, ObjectKind::Ordinary)?.into())
    }

    /// GetIteratorDirect。
    fn direct<'gc>(a: &mut Agent<'gc, '_>, o: Value<'gc>) -> Result<IterRecord<'gc>, Error> {
        let next = a.get_named(o, "next")?;
        Ok(IterRecord { iterator: o, next })
    }

    /// `this` 要是对象。
    fn this_object<'gc>(a: &Agent<'gc, '_>, this: Value<'gc>) -> Result<Value<'gc>, Error> {
        match this {
            Value::Object(_) => Ok(this),
            _ => Err(a.throw(ErrorKind::Type, "Iterator.prototype method called on a non-object")),
        }
    }

    /// 实参校验失败：先关掉 `this` 这个迭代器再抛。
    fn reject<'gc>(a: &mut Agent<'gc, '_>, iterator: Value<'gc>, kind: ErrorKind, message: &str) -> Error {
        let e = a.throw(kind, message);
        let _ = a.iterator_close(iterator);
        e
    }

    fn callable<'gc>(a: &mut Agent<'gc, '_>, iterator: Value<'gc>, f: Value<'gc>) -> Result<(), Error> {
        if a.is_callable(f) {
            Ok(())
        } else {
            let t = a.describe(f);
            Err(Self::reject(a, iterator, ErrorKind::Type, &alloc::format!("{t} is not a function")))
        }
    }

    /// 出错时悄悄关掉底层迭代器，原错误照抛。
    fn close_on<'gc, T>(a: &mut Agent<'gc, '_>, iterator: Value<'gc>, r: Result<T, Error>) -> Result<T, Error> {
        if r.is_err() {
            let _ = a.iterator_close(iterator);
        }
        r
    }

    fn make<'gc>(a: &mut Agent<'gc, '_>, this: Value<'gc>, kind: HelperKind, f: Value<'gc>, remaining: f64) -> Result<Value<'gc>, Error> {
        let underlying = Self::direct(a, this)?;
        let h = IteratorHelper { kind, state: HelperState::Start, underlying, f, remaining, counter: 0.0, inner: None };
        Ok(ObjectData { kind: ObjectKind::IteratorHelper(h), proto: Some(a.realm.iterator_helper_proto), ..ObjectData::default() }.alloc(a.mc).into())
    }

    fn with_callback<'gc>(a: &mut Agent<'gc, '_>, this: Value<'gc>, args: &[Value<'gc>], kind: HelperKind) -> Result<Value<'gc>, Error> {
        let this = Self::this_object(a, this)?;
        Self::callable(a, this, args.at(0))?;
        Self::make(a, this, kind, args.at(0), 0.0)
    }

    fn map<'gc>(a: &mut Agent<'gc, '_>, this: Value<'gc>, args: &[Value<'gc>]) -> Result<Value<'gc>, Error> {
        Self::with_callback(a, this, args, HelperKind::Map)
    }

    fn filter<'gc>(a: &mut Agent<'gc, '_>, this: Value<'gc>, args: &[Value<'gc>]) -> Result<Value<'gc>, Error> {
        Self::with_callback(a, this, args, HelperKind::Filter)
    }

    fn flat_map<'gc>(a: &mut Agent<'gc, '_>, this: Value<'gc>, args: &[Value<'gc>]) -> Result<Value<'gc>, Error> {
        Self::with_callback(a, this, args, HelperKind::FlatMap)
    }

    /// `take` / `drop` 的个数：不是 NaN、不是负数（可以是 +∞）。
    fn limited<'gc>(a: &mut Agent<'gc, '_>, this: Value<'gc>, args: &[Value<'gc>], kind: HelperKind) -> Result<Value<'gc>, Error> {
        let this = Self::this_object(a, this)?;
        let n = a.to_number(args.at(0));
        let n = Self::close_on(a, this, n)?;
        if n.is_nan() {
            return Err(Self::reject(a, this, ErrorKind::Range, "limit must be a number"));
        }
        let n = a.to_integer_or_infinity(n.into())?;
        if n < 0.0 {
            return Err(Self::reject(a, this, ErrorKind::Range, "limit must not be negative"));
        }
        Self::make(a, this, kind, Value::Undefined, n)
    }

    fn take<'gc>(a: &mut Agent<'gc, '_>, this: Value<'gc>, args: &[Value<'gc>]) -> Result<Value<'gc>, Error> {
        Self::limited(a, this, args, HelperKind::Take)
    }

    fn drop<'gc>(a: &mut Agent<'gc, '_>, this: Value<'gc>, args: &[Value<'gc>]) -> Result<Value<'gc>, Error> {
        Self::limited(a, this, args, HelperKind::Drop)
    }

    /// GetIteratorFlattenable：`strings` 时字符串也行，别的原始值抛 TypeError。
    fn flattenable<'gc>(a: &mut Agent<'gc, '_>, v: Value<'gc>, strings: bool) -> Result<IterRecord<'gc>, Error> {
        match v {
            Value::Object(_) => {}
            Value::String(_) if strings => {}
            _ => {
                let t = a.describe(v);
                return Err(a.throw(ErrorKind::Type, &alloc::format!("{t} is not an object")));
            }
        }
        let method = a.get_symbol(v, a.realm.symbols.iterator, v)?;
        let iterator = if method.is_nullish() { v } else { a.call(method, v, &[])? };
        if !matches!(iterator, Value::Object(_)) {
            return Err(a.throw(ErrorKind::Type, "the iterator is not an object"));
        }
        Self::direct(a, iterator)
    }

    /// 助手跑一步：给出产出的值，走完给 `None`。
    fn step<'gc>(a: &mut Agent<'gc, '_>, h: &mut IteratorHelper<'gc>) -> Result<Option<Value<'gc>>, Error> {
        let under = h.underlying;
        loop {
            match h.kind {
                HelperKind::Take if h.remaining == 0.0 => {
                    a.iterator_close(under.iterator)?;
                    return Ok(None);
                }
                HelperKind::Drop => {
                    while h.remaining > 0.0 {
                        if h.remaining != f64::INFINITY {
                            h.remaining -= 1.0;
                        }
                        if a.iterator_step(under)?.is_none() {
                            return Ok(None);
                        }
                    }
                }
                HelperKind::FlatMap => {
                    if let Some(inner) = h.inner {
                        let r = a.iterator_step(inner);
                        match Self::close_on(a, under.iterator, r)? {
                            Some(v) => return Ok(Some(v)),
                            None => {
                                h.inner = None;
                                continue;
                            }
                        }
                    }
                }
                _ => {}
            }
            if h.kind == HelperKind::Take && h.remaining != f64::INFINITY {
                h.remaining -= 1.0;
            }
            let Some(v) = a.iterator_step(under)? else { return Ok(None) };
            let counter = h.counter;
            match h.kind {
                HelperKind::Take | HelperKind::Drop => return Ok(Some(v)),
                HelperKind::Map => {
                    let r = a.call(h.f, Value::Undefined, &[v, counter.into()]);
                    let mapped = Self::close_on(a, under.iterator, r)?;
                    h.counter += 1.0;
                    return Ok(Some(mapped));
                }
                HelperKind::Filter => {
                    let r = a.call(h.f, Value::Undefined, &[v, counter.into()]);
                    let selected = Self::close_on(a, under.iterator, r)?;
                    h.counter += 1.0;
                    if a.to_boolean(selected) {
                        return Ok(Some(v));
                    }
                }
                HelperKind::FlatMap => {
                    let r = a.call(h.f, Value::Undefined, &[v, counter.into()]).and_then(|m| Self::flattenable(a, m, false));
                    h.inner = Some(Self::close_on(a, under.iterator, r)?);
                    h.counter += 1.0;
                }
            }
        }
    }

    fn helper<'gc>(a: &Agent<'gc, '_>, this: Value<'gc>) -> Result<(JsObject<'gc>, IteratorHelper<'gc>), Error> {
        match this {
            Value::Object(o) => match o.borrow().kind {
                ObjectKind::IteratorHelper(h) => Ok((o, h)),
                _ => Err(a.throw(ErrorKind::Type, "Iterator Helper method called on an incompatible receiver")),
            },
            _ => Err(a.throw(ErrorKind::Type, "Iterator Helper method called on an incompatible receiver")),
        }
    }

    fn store<'gc>(a: &Agent<'gc, '_>, o: JsObject<'gc>, h: IteratorHelper<'gc>) {
        if let ObjectKind::IteratorHelper(x) = &mut o.borrow_mut(a.mc).kind {
            *x = h;
        }
    }

    fn helper_next<'gc>(a: &mut Agent<'gc, '_>, this: Value<'gc>, _: &[Value<'gc>]) -> Result<Value<'gc>, Error> {
        let (o, mut h) = Self::helper(a, this)?;
        match h.state {
            HelperState::Running => return Err(a.throw(ErrorKind::Type, "Generator is already running")),
            HelperState::Done => return Ok(a.iter_result(Value::Undefined, true).into()),
            HelperState::Start | HelperState::Suspended => {}
        }
        Self::store(a, o, IteratorHelper { state: HelperState::Running, ..h });
        let r = Self::step(a, &mut h);
        h.state = if matches!(r, Ok(Some(_))) { HelperState::Suspended } else { HelperState::Done };
        Self::store(a, o, h);
        match r? {
            Some(v) => Ok(a.iter_result(v, false).into()),
            None => Ok(a.iter_result(Value::Undefined, true).into()),
        }
    }

    fn helper_return<'gc>(a: &mut Agent<'gc, '_>, this: Value<'gc>, _: &[Value<'gc>]) -> Result<Value<'gc>, Error> {
        let (o, h) = Self::helper(a, this)?;
        match h.state {
            HelperState::Running => return Err(a.throw(ErrorKind::Type, "Generator is already running")),
            HelperState::Done => return Ok(a.iter_result(Value::Undefined, true).into()),
            HelperState::Start | HelperState::Suspended => {}
        }
        Self::store(a, o, IteratorHelper { state: HelperState::Done, ..h });
        if let (HelperState::Suspended, Some(inner)) = (h.state, h.inner) {
            let r = a.iterator_close(inner.iterator);
            Self::close_on(a, h.underlying.iterator, r)?;
        }
        a.iterator_close(h.underlying.iterator)?;
        Ok(a.iter_result(Value::Undefined, true).into())
    }

    /// 消费型方法的开头：校验回调、取 `next`。
    fn consumer<'gc>(a: &mut Agent<'gc, '_>, this: Value<'gc>, f: Value<'gc>) -> Result<IterRecord<'gc>, Error> {
        let this = Self::this_object(a, this)?;
        Self::callable(a, this, f)?;
        Self::direct(a, this)
    }

    fn reduce<'gc>(a: &mut Agent<'gc, '_>, this: Value<'gc>, args: &[Value<'gc>]) -> Result<Value<'gc>, Error> {
        let f = args.at(0);
        let it = Self::consumer(a, this, f)?;
        let (mut acc, mut counter) = if args.len() >= 2 {
            (args[1], 0.0)
        } else {
            match a.iterator_step(it)? {
                Some(v) => (v, 1.0),
                None => return Err(a.throw(ErrorKind::Type, "Reduce of empty iterator with no initial value")),
            }
        };
        while let Some(v) = a.iterator_step(it)? {
            let r = a.call(f, Value::Undefined, &[acc, v, counter.into()]);
            acc = Self::close_on(a, it.iterator, r)?;
            counter += 1.0;
        }
        Ok(acc)
    }

    fn to_array<'gc>(a: &mut Agent<'gc, '_>, this: Value<'gc>, _: &[Value<'gc>]) -> Result<Value<'gc>, Error> {
        let this = Self::this_object(a, this)?;
        let it = Self::direct(a, this)?;
        let mut out = Vec::new();
        while let Some(v) = a.iterator_step(it)? {
            out.push(v);
        }
        Ok(a.array(out).into())
    }

    fn for_each<'gc>(a: &mut Agent<'gc, '_>, this: Value<'gc>, args: &[Value<'gc>]) -> Result<Value<'gc>, Error> {
        let f = args.at(0);
        let it = Self::consumer(a, this, f)?;
        let mut counter = 0.0;
        while let Some(v) = a.iterator_step(it)? {
            let r = a.call(f, Value::Undefined, &[v, Value::from(counter)]);
            Self::close_on(a, it.iterator, r)?;
            counter += 1.0;
        }
        Ok(Value::Undefined)
    }

    fn probe<'gc>(a: &mut Agent<'gc, '_>, this: Value<'gc>, args: &[Value<'gc>], how: Probe) -> Result<Value<'gc>, Error> {
        let f = args.at(0);
        let it = Self::consumer(a, this, f)?;
        let mut counter = 0.0;
        while let Some(v) = a.iterator_step(it)? {
            let r = a.call(f, Value::Undefined, &[v, Value::from(counter)]);
            let hit = Self::close_on(a, it.iterator, r)?;
            let hit = a.to_boolean(hit);
            let out = match (how, hit) {
                (Probe::Some, true) => Some(Value::Bool(true)),
                (Probe::Every, false) => Some(Value::Bool(false)),
                (Probe::Find, true) => Some(v),
                _ => None,
            };
            if let Some(out) = out {
                a.iterator_close(it.iterator)?;
                return Ok(out);
            }
            counter += 1.0;
        }
        Ok(match how {
            Probe::Some => false.into(),
            Probe::Every => true.into(),
            Probe::Find => Value::Undefined,
        })
    }

    fn some<'gc>(a: &mut Agent<'gc, '_>, this: Value<'gc>, args: &[Value<'gc>]) -> Result<Value<'gc>, Error> {
        Self::probe(a, this, args, Probe::Some)
    }

    fn every<'gc>(a: &mut Agent<'gc, '_>, this: Value<'gc>, args: &[Value<'gc>]) -> Result<Value<'gc>, Error> {
        Self::probe(a, this, args, Probe::Every)
    }

    fn find<'gc>(a: &mut Agent<'gc, '_>, this: Value<'gc>, args: &[Value<'gc>]) -> Result<Value<'gc>, Error> {
        Self::probe(a, this, args, Probe::Find)
    }

    /// `Iterator.from(o)`：已经继承 Iterator.prototype 的原样给，否则包一层。
    fn from<'gc>(a: &mut Agent<'gc, '_>, _: Value<'gc>, args: &[Value<'gc>]) -> Result<Value<'gc>, Error> {
        let rec = Self::flattenable(a, args.at(0), true)?;
        let inherits = match rec.iterator {
            Value::Object(it) => {
                let mut cur = a.get_prototype_of(it)?;
                let mut found = false;
                while let Some(p) = cur {
                    if r8gc::Gc::ptr_eq(p, a.realm.iterator_proto) {
                        found = true;
                        break;
                    }
                    cur = a.get_prototype_of(p)?;
                }
                found
            }
            _ => false,
        };
        if inherits {
            return Ok(rec.iterator);
        }
        Ok(ObjectData { kind: ObjectKind::WrappedIterator(rec), proto: Some(a.realm.wrap_iterator_proto), ..ObjectData::default() }.alloc(a.mc).into())
    }

    fn wrapped<'gc>(a: &Agent<'gc, '_>, this: Value<'gc>) -> Result<IterRecord<'gc>, Error> {
        match this {
            Value::Object(o) => match o.borrow().kind {
                ObjectKind::WrappedIterator(r) => Ok(r),
                _ => Err(a.throw(ErrorKind::Type, "method called on an incompatible receiver")),
            },
            _ => Err(a.throw(ErrorKind::Type, "method called on an incompatible receiver")),
        }
    }

    fn wrapped_next<'gc>(a: &mut Agent<'gc, '_>, this: Value<'gc>, _: &[Value<'gc>]) -> Result<Value<'gc>, Error> {
        let r = Self::wrapped(a, this)?;
        a.call(r.next, r.iterator, &[])
    }

    fn wrapped_return<'gc>(a: &mut Agent<'gc, '_>, this: Value<'gc>, _: &[Value<'gc>]) -> Result<Value<'gc>, Error> {
        let r = Self::wrapped(a, this)?;
        let ret = a.get_named(r.iterator, "return")?;
        if ret.is_nullish() {
            return Ok(a.iter_result(Value::Undefined, true).into());
        }
        a.call(ret, r.iterator, &[])
    }

    /// `Iterator.prototype[Symbol.toStringTag]` 的 getter。
    fn tag_get<'gc>(a: &mut Agent<'gc, '_>, _: Value<'gc>, _: &[Value<'gc>]) -> Result<Value<'gc>, Error> {
        Ok(a.string("Iterator").into())
    }

    /// `Iterator.prototype.constructor` 的 getter。
    fn ctor_get<'gc>(a: &mut Agent<'gc, '_>, _: Value<'gc>, _: &[Value<'gc>]) -> Result<Value<'gc>, Error> {
        Ok(a.realm.global.borrow().own("Iterator").unwrap_or_default())
    }

    /// SetterThatIgnoresPrototypeProperties：在 Iterator.prototype 本身上写抛 TypeError，别的对象上建 / 改自有属性。
    fn ignoring_setter<'gc>(a: &mut Agent<'gc, '_>, this: Value<'gc>, key: Key<'_, 'gc>, v: Value<'gc>) -> Result<Value<'gc>, Error> {
        let Value::Object(o) = this else {
            return Err(a.throw(ErrorKind::Type, "setter called on a non-object"));
        };
        if r8gc::Gc::ptr_eq(o, a.realm.iterator_proto) {
            return Err(a.throw(ErrorKind::Type, "Cannot assign to this property of Iterator.prototype"));
        }
        if a.get_own_property(o, key)?.is_none() {
            a.create_data_property_or_throw(o, key, v)?;
        } else if !a.set_key(this, key, v, this)? {
            let k = a.key_text(key);
            return Err(a.throw(ErrorKind::Type, &alloc::format!("Cannot assign to read only property '{k}'")));
        }
        Ok(Value::Undefined)
    }

    fn tag_set<'gc>(a: &mut Agent<'gc, '_>, this: Value<'gc>, args: &[Value<'gc>]) -> Result<Value<'gc>, Error> {
        Self::ignoring_setter(a, this, Key::Sym(a.realm.symbols.to_string_tag), args.at(0))
    }

    fn ctor_set<'gc>(a: &mut Agent<'gc, '_>, this: Value<'gc>, args: &[Value<'gc>]) -> Result<Value<'gc>, Error> {
        Self::ignoring_setter(a, this, Key::Str("constructor"), args.at(0))
    }
}

impl Installer<'_, '_> {
    pub(crate) fn helpers(&mut self) {
        let r = self.realm;
        let proto = r.iterator_proto;
        let ctor = r.named_native(self.mc, Helpers::iterator, "Iterator", 0);
        if let ObjectKind::Native(n) = &mut ctor.borrow_mut(self.mc).kind {
            n.constructor = true;
        }
        ctor.borrow_mut(self.mc).define("prototype", Property { slot: Slot::Data(proto.into()), enumerable: false, writable: false, configurable: false });
        self.set(r.global, "Iterator", ctor.into());
        self.method(ctor, "from", Helpers::from, 1);
        let methods: [(&str, r8frame::NativeFn, u32); 11] = [
            ("map", Helpers::map, 1),
            ("filter", Helpers::filter, 1),
            ("take", Helpers::take, 1),
            ("drop", Helpers::drop, 1),
            ("flatMap", Helpers::flat_map, 1),
            ("reduce", Helpers::reduce, 1),
            ("toArray", Helpers::to_array, 0),
            ("forEach", Helpers::for_each, 1),
            ("some", Helpers::some, 1),
            ("every", Helpers::every, 1),
            ("find", Helpers::find, 1),
        ];
        for (name, f, n) in methods {
            self.method(proto, name, f, n);
        }
        let accessor = |get: r8frame::NativeFn, set: r8frame::NativeFn, name: &str| {
            let g = r.named_native(self.mc, get, &alloc::format!("get {name}"), 0);
            let s = r.named_native(self.mc, set, &alloc::format!("set {name}"), 1);
            Property { slot: Slot::Accessor { get: Some(g), set: Some(s) }, enumerable: false, writable: false, configurable: true }
        };
        let ctor_prop = accessor(Helpers::ctor_get, Helpers::ctor_set, "constructor");
        proto.borrow_mut(self.mc).define("constructor", ctor_prop);
        let tag_prop = accessor(Helpers::tag_get, Helpers::tag_set, "[Symbol.toStringTag]");
        proto.borrow_mut(self.mc).define_symbol(r.symbols.to_string_tag, tag_prop);
        self.method(r.iterator_helper_proto, "next", Helpers::helper_next, 0);
        self.method(r.iterator_helper_proto, "return", Helpers::helper_return, 0);
        self.method(r.wrap_iterator_proto, "next", Helpers::wrapped_next, 0);
        self.method(r.wrap_iterator_proto, "return", Helpers::wrapped_return, 0);
    }
}
