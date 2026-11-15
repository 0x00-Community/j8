use r8framework::{Agent, Error, ErrorKind, IterKind, IteratorState, ObjectKind, Resume, Step, SymbolData, Value};

use crate::{Args, Installer};

/// `Symbol`, built-in iterators, generator prototypes.
struct Iters;

impl Iters {
    /// `Symbol(description)`.
    fn symbol<'gc>(a: &mut Agent<'gc, '_>, _: Value<'gc>, args: &[Value<'gc>]) -> Result<Value<'gc>, Error> {
        let description = match args.at(0) {
            Value::Undefined => None,
            d => Some((&**a.to_string(d)?).into()),
        };
        Ok(Value::Symbol(r8gc::Gc::new(a.mc, SymbolData { description })))
    }

    fn this_symbol<'gc>(a: &Agent<'gc, '_>, this: Value<'gc>) -> Result<r8framework::JsSymbol<'gc>, Error> {
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

    /// thisSymbolValue (`Symbol.prototype.valueOf`)
    fn symbol_value_of<'gc>(a: &mut Agent<'gc, '_>, this: Value<'gc>, _: &[Value<'gc>]) -> Result<Value<'gc>, Error> {
        Ok(Self::this_symbol(a, this)?.into())
    }

    fn this_value<'gc>(_: &mut Agent<'gc, '_>, this: Value<'gc>, _: &[Value<'gc>]) -> Result<Value<'gc>, Error> {
        Ok(this)
    }

    fn values<'gc>(a: &mut Agent<'gc, '_>, this: Value<'gc>, _: &[Value<'gc>]) -> Result<Value<'gc>, Error> {
        let o = a.to_object(this)?;
        Ok(a.array_iterator(o.into(), IterKind::Values).into())
    }

    fn keys<'gc>(a: &mut Agent<'gc, '_>, this: Value<'gc>, _: &[Value<'gc>]) -> Result<Value<'gc>, Error> {
        let o = a.to_object(this)?;
        Ok(a.array_iterator(o.into(), IterKind::Keys).into())
    }

    fn entries<'gc>(a: &mut Agent<'gc, '_>, this: Value<'gc>, _: &[Value<'gc>]) -> Result<Value<'gc>, Error> {
        let o = a.to_object(this)?;
        Ok(a.array_iterator(o.into(), IterKind::Entries).into())
    }

    fn string_iterator<'gc>(a: &mut Agent<'gc, '_>, this: Value<'gc>, _: &[Value<'gc>]) -> Result<Value<'gc>, Error> {
        if this.is_nullish() {
            return Err(a.throw(ErrorKind::Type, "String.prototype[Symbol.iterator] called on null or undefined"));
        }
        let s = a.to_string(this)?;
        Ok(a.builtin_iterator(IteratorState::String { s, pos: 0 }).into())
    }

    fn state<'gc>(a: &Agent<'gc, '_>, this: Value<'gc>) -> Result<r8framework::JsObject<'gc>, Error> {
        match this {
            Value::Object(o) if matches!(o.borrow().kind, ObjectKind::Iterator(_)) => Ok(o),
            _ => Err(a.throw(ErrorKind::Type, "next method called on an incompatible receiver")),
        }
    }

    /// `next` of array iterators: re-reads `length` each time (changes to the array during iteration are visible).
    fn array_next<'gc>(a: &mut Agent<'gc, '_>, this: Value<'gc>, _: &[Value<'gc>]) -> Result<Value<'gc>, Error> {
        let o = Self::state(a, this)?;
        let v = a.array_iter_step(o)?;
        Ok(a.iter_result(v.unwrap_or_default(), v.is_none()).into())
    }

    /// `next` of string iterators: one code point at a time.
    fn string_next<'gc>(a: &mut Agent<'gc, '_>, this: Value<'gc>, _: &[Value<'gc>]) -> Result<Value<'gc>, Error> {
        let o = Self::state(a, this)?;
        let v = a.string_iter_step(o)?;
        Ok(a.iter_result(v.unwrap_or_default(), v.is_none()).into())
    }

    /// Generator `next` / `return` / `throw`: resume one step, wrap the result in `{ value, done }`.
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
        let ctor = self.constructor("Symbol", Iters::symbol, proto, 0);
        self.not_constructor(ctor);
        for (name, s) in sym.all() {
            self.set(ctor, name, Value::Symbol(s));
        }
        self.method(proto, "toString", Iters::symbol_to_string, 0);
        self.method(proto, "valueOf", Iters::symbol_value_of, 0);
        self.getter(proto, "description", Iters::symbol_description);
        r.symbol_method(self.mc, r.iterator_proto, sym.iterator, Iters::this_value, 0);
        // Array: `values` and `[Symbol.iterator]` are the same function
        let values = r.named_native(self.mc, Iters::values, "values", 0);
        self.set(r.array_proto, "values", values.into());
        r.array_proto.borrow_mut(self.mc).define_symbol(sym.iterator, r8framework::Property::hidden(values.into()));
        self.method(r.array_proto, "keys", Iters::keys, 0);
        self.method(r.array_proto, "entries", Iters::entries, 0);
        self.method(r.array_iterator_proto, "next", Iters::array_next, 0);
        r.symbol_method(self.mc, r.string_proto, sym.iterator, Iters::string_iterator, 0);
        self.method(r.string_iterator_proto, "next", Iters::string_next, 0);
        // Record the built-in `next`s: when an iterator record's `next` is one of them, the execution layer steps
        // directly without creating a result object
        let nexts = [r.array_iterator_proto, r.string_iterator_proto].map(|o| match o.borrow().own("next") {
            Some(Value::Object(f)) => Some(f),
            _ => None,
        });
        r.builtin_nexts.set(self.mc, nexts);
        self.method(r.generator_proto, "next", Iters::generator_next, 1);
        self.method(r.generator_proto, "return", Iters::generator_return, 1);
        self.method(r.generator_proto, "throw", Iters::generator_throw, 1);
    }
}
