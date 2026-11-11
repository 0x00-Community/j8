use r8frame::{Agent, Error, ErrorKind, IterKind, IteratorState, ObjectKind, Resume, Step, SymbolData, Value};

use crate::{Args, Installer};

/// `Symbol`、内建迭代器、生成器原型。
struct Iters;

impl Iters {
    /// `Symbol(描述)`。
    fn symbol<'gc>(a: &mut Agent<'gc, '_>, _: Value<'gc>, args: &[Value<'gc>]) -> Result<Value<'gc>, Error> {
        let description = match args.at(0) {
            Value::Undefined => None,
            d => Some((&**a.to_string(d)?).into()),
        };
        Ok(Value::Symbol(gc_arena::Gc::new(a.mc, SymbolData { description })))
    }

    fn this_symbol<'gc>(a: &Agent<'gc, '_>, this: Value<'gc>) -> Result<r8frame::JsSymbol<'gc>, Error> {
        match this.unwrapped() {
            Value::Symbol(s) => Ok(s),
            _ => Err(a.throw(ErrorKind::Type, "Symbol.prototype method called on a non-symbol")),
        }
    }

    fn symbol_to_string<'gc>(a: &mut Agent<'gc, '_>, this: Value<'gc>, _: &[Value<'gc>]) -> Result<Value<'gc>, Error> {
        let s = Self::this_symbol(a, this)?;
        Ok(a.symbol_descriptive(s).into())
    }

    fn symbol_description<'gc>(a: &mut Agent<'gc, '_>, this: Value<'gc>, _: &[Value<'gc>]) -> Result<Value<'gc>, Error> {
        let s = Self::this_symbol(a, this)?;
        Ok(s.description.as_deref().map_or(Value::Undefined, |d| a.string(d).into()))
    }

    /// thisSymbolValue（`Symbol.prototype.valueOf`）
    fn symbol_value_of<'gc>(a: &mut Agent<'gc, '_>, this: Value<'gc>, _: &[Value<'gc>]) -> Result<Value<'gc>, Error> {
        Ok(Self::this_symbol(a, this)?.into())
    }

    fn this_value<'gc>(_: &mut Agent<'gc, '_>, this: Value<'gc>, _: &[Value<'gc>]) -> Result<Value<'gc>, Error> {
        Ok(this)
    }

    fn values<'gc>(a: &mut Agent<'gc, '_>, this: Value<'gc>, _: &[Value<'gc>]) -> Result<Value<'gc>, Error> {
        Ok(a.array_iterator(this, IterKind::Values).into())
    }

    fn keys<'gc>(a: &mut Agent<'gc, '_>, this: Value<'gc>, _: &[Value<'gc>]) -> Result<Value<'gc>, Error> {
        Ok(a.array_iterator(this, IterKind::Keys).into())
    }

    fn entries<'gc>(a: &mut Agent<'gc, '_>, this: Value<'gc>, _: &[Value<'gc>]) -> Result<Value<'gc>, Error> {
        Ok(a.array_iterator(this, IterKind::Entries).into())
    }

    fn string_iterator<'gc>(a: &mut Agent<'gc, '_>, this: Value<'gc>, _: &[Value<'gc>]) -> Result<Value<'gc>, Error> {
        if this.is_nullish() {
            return Err(a.throw(ErrorKind::Type, "String.prototype[Symbol.iterator] called on null or undefined"));
        }
        let s = a.to_string(this)?;
        Ok(a.builtin_iterator(IteratorState::String { s, pos: 0 }).into())
    }

    fn state<'gc>(a: &Agent<'gc, '_>, this: Value<'gc>) -> Result<r8frame::JsObject<'gc>, Error> {
        match this {
            Value::Object(o) if matches!(o.borrow().kind, ObjectKind::Iterator(_)) => Ok(o),
            _ => Err(a.throw(ErrorKind::Type, "next method called on an incompatible receiver")),
        }
    }

    /// 数组迭代器的 `next`：每次重读 `length`（迭代中改数组看得见）。
    fn array_next<'gc>(a: &mut Agent<'gc, '_>, this: Value<'gc>, _: &[Value<'gc>]) -> Result<Value<'gc>, Error> {
        let o = Self::state(a, this)?;
        let ObjectKind::Iterator(IteratorState::Array { target, index, kind }) = o.borrow().kind else {
            return Err(a.throw(ErrorKind::Type, "next method called on an incompatible receiver"));
        };
        let Some(target) = target else { return Ok(a.iter_result(Value::Undefined, true).into()) };
        let len = a.get_named(target, "length")?;
        let len = a.to_number(len)?;
        let i = f64::from(u32::try_from(index).unwrap_or(u32::MAX));
        // len 是 NaN 时也算走完
        let finished = i.partial_cmp(&len) != Some(core::cmp::Ordering::Less);
        if let ObjectKind::Iterator(IteratorState::Array { target, index, .. }) = &mut o.borrow_mut(a.mc).kind {
            if finished {
                *target = None;
            } else {
                *index += 1;
            }
        }
        if finished {
            return Ok(a.iter_result(Value::Undefined, true).into());
        }
        let v = match kind {
            IterKind::Keys => i.into(),
            IterKind::Values => a.get_keyed(target, i.into())?,
            IterKind::Entries => {
                let v = a.get_keyed(target, i.into())?;
                a.array(alloc::vec![i.into(), v]).into()
            }
        };
        Ok(a.iter_result(v, false).into())
    }

    /// 字符串迭代器的 `next`：一次一个码点。
    fn string_next<'gc>(a: &mut Agent<'gc, '_>, this: Value<'gc>, _: &[Value<'gc>]) -> Result<Value<'gc>, Error> {
        let o = Self::state(a, this)?;
        let ObjectKind::Iterator(IteratorState::String { s, pos }) = o.borrow().kind else {
            return Err(a.throw(ErrorKind::Type, "next method called on an incompatible receiver"));
        };
        let Some(c) = s[pos..].chars().next() else { return Ok(a.iter_result(Value::Undefined, true).into()) };
        if let ObjectKind::Iterator(IteratorState::String { pos, .. }) = &mut o.borrow_mut(a.mc).kind {
            *pos += c.len_utf8();
        }
        let v = a.string(c.encode_utf8(&mut [0; 4]));
        Ok(a.iter_result(v.into(), false).into())
    }

    /// 生成器的 `next` / `return` / `throw`：恢复一步，包成 `{ value, done }`。
    fn resume<'gc>(a: &mut Agent<'gc, '_>, this: Value<'gc>, mode: Resume, v: Value<'gc>) -> Result<Value<'gc>, Error> {
        let co = match this {
            Value::Object(o) if matches!(&o.borrow().kind, ObjectKind::Coroutine(c) if c.promise.is_none() && c.requests.is_none()) => o,
            _ => return Err(a.throw(ErrorKind::Type, "Generator method called on an incompatible receiver")),
        };
        Ok(match a.resume(co, mode, v)? {
            Step::Yield(x) => a.iter_result(x, false),
            Step::Done(x) => a.iter_result(x, true),
            Step::Await(_) => unreachable!("generators do not await"),
        }
        .into())
    }

    fn generator_next<'gc>(a: &mut Agent<'gc, '_>, this: Value<'gc>, args: &[Value<'gc>]) -> Result<Value<'gc>, Error> {
        Self::resume(a, this, Resume::Next, args.at(0))
    }

    fn generator_return<'gc>(a: &mut Agent<'gc, '_>, this: Value<'gc>, args: &[Value<'gc>]) -> Result<Value<'gc>, Error> {
        Self::resume(a, this, Resume::Return, args.at(0))
    }

    fn generator_throw<'gc>(a: &mut Agent<'gc, '_>, this: Value<'gc>, args: &[Value<'gc>]) -> Result<Value<'gc>, Error> {
        Self::resume(a, this, Resume::Throw, args.at(0))
    }
}

impl Installer<'_, '_> {
    pub(crate) fn iterators(&mut self) {
        let r = self.realm;
        let sym = r.symbols;
        let proto = r.symbol_proto;
        let ctor = self.constructor("Symbol", Iters::symbol, proto);
        for (name, s) in sym.all() {
            self.set(ctor, name, Value::Symbol(s));
        }
        self.method(proto, "toString", Iters::symbol_to_string);
        self.method(proto, "valueOf", Iters::symbol_value_of);
        self.getter(proto, "description", Iters::symbol_description);
        r.symbol_method(self.mc, r.iterator_proto, sym.iterator, Iters::this_value);
        // 数组：`values` 与 `[Symbol.iterator]` 是同一个函数
        let values = r.named_native(self.mc, Iters::values, "values");
        self.set(r.array_proto, "values", values.into());
        r.array_proto.borrow_mut(self.mc).define_symbol(sym.iterator, r8frame::Property::hidden(values.into()));
        self.method(r.array_proto, "keys", Iters::keys);
        self.method(r.array_proto, "entries", Iters::entries);
        self.method(r.array_iterator_proto, "next", Iters::array_next);
        r.symbol_method(self.mc, r.string_proto, sym.iterator, Iters::string_iterator);
        self.method(r.string_iterator_proto, "next", Iters::string_next);
        self.method(r.generator_proto, "next", Iters::generator_next);
        self.method(r.generator_proto, "return", Iters::generator_return);
        self.method(r.generator_proto, "throw", Iters::generator_throw);
    }
}
