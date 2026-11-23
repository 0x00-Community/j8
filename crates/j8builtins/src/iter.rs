use j8framework::ValueKind;
use j8framework::{Agent, Error, ErrorKind, IterKind, IteratorState, ObjectKind, Resume, Step, SymbolData, Value};

use crate::{Args, Installer};

struct Iters;

impl Iters {
    fn symbol<'scope>(a: &mut Agent<'scope, '_>, _: Value<'scope>, args: &[Value<'scope>]) -> Result<Value<'scope>, Error> {
        if a.new_target != Value::Undefined {
            return Err(a.throw(ErrorKind::Type, "Symbol is not a constructor"));
        }
        let description = match args.at(0).kind() {
            ValueKind::Undefined => None,
            d => Some((&**a.to_string(Value::from(d))?).into()),
        };
        Ok(Value::Symbol(a.mc.alloc(SymbolData { description, private: false })))
    }

    fn this_symbol<'scope>(a: &Agent<'scope, '_>, this: Value<'scope>) -> Result<j8framework::JsSymbol<'scope>, Error> {
        match this.unwrapped().kind() {
            ValueKind::Symbol(s) => Ok(s),
            _ => Err(a.throw(ErrorKind::Type, "Symbol.prototype method called on a non-symbol")),
        }
    }

    fn symbol_to_string<'scope>(a: &mut Agent<'scope, '_>, this: Value<'scope>, _: &[Value<'scope>]) -> Result<Value<'scope>, Error> {
        let s = Self::this_symbol(a, this)?;
        Ok(a.symbol_descriptive(s).into())
    }

    fn symbol_description<'scope>(a: &mut Agent<'scope, '_>, this: Value<'scope>, _: &[Value<'scope>]) -> Result<Value<'scope>, Error> {
        let s = Self::this_symbol(a, this)?;
        Ok(s.description.as_deref().map_or(Value::Undefined, |d| a.string(d).into()))
    }

    fn symbol_value_of<'scope>(a: &mut Agent<'scope, '_>, this: Value<'scope>, _: &[Value<'scope>]) -> Result<Value<'scope>, Error> {
        Ok(Self::this_symbol(a, this)?.into())
    }

    fn this_value<'scope>(_: &mut Agent<'scope, '_>, this: Value<'scope>, _: &[Value<'scope>]) -> Result<Value<'scope>, Error> {
        Ok(this)
    }

    fn values<'scope>(a: &mut Agent<'scope, '_>, this: Value<'scope>, _: &[Value<'scope>]) -> Result<Value<'scope>, Error> {
        let o = a.to_object(this)?;
        Ok(a.array_iterator(o.into(), IterKind::Values).into())
    }

    fn keys<'scope>(a: &mut Agent<'scope, '_>, this: Value<'scope>, _: &[Value<'scope>]) -> Result<Value<'scope>, Error> {
        let o = a.to_object(this)?;
        Ok(a.array_iterator(o.into(), IterKind::Keys).into())
    }

    fn entries<'scope>(a: &mut Agent<'scope, '_>, this: Value<'scope>, _: &[Value<'scope>]) -> Result<Value<'scope>, Error> {
        let o = a.to_object(this)?;
        Ok(a.array_iterator(o.into(), IterKind::Entries).into())
    }

    fn string_iterator<'scope>(a: &mut Agent<'scope, '_>, this: Value<'scope>, _: &[Value<'scope>]) -> Result<Value<'scope>, Error> {
        if this.is_nullish() {
            return Err(a.throw(ErrorKind::Type, "String.prototype[Symbol.iterator] called on null or undefined"));
        }
        let s = a.to_string(this)?;
        Ok(a.builtin_iterator(IteratorState::String { s, pos: 0 }).into())
    }

    fn state<'scope>(a: &Agent<'scope, '_>, this: Value<'scope>) -> Result<j8framework::JsObject<'scope>, Error> {
        match this.kind() {
            ValueKind::Object(o) if matches!(o.borrow().kind, ObjectKind::Iterator(_)) => Ok(o),
            _ => Err(a.throw(ErrorKind::Type, "next method called on an incompatible receiver")),
        }
    }

    fn array_next<'scope>(a: &mut Agent<'scope, '_>, this: Value<'scope>, _: &[Value<'scope>]) -> Result<Value<'scope>, Error> {
        let o = Self::state(a, this)?;
        let v = a.array_iter_step(o)?;
        Ok(a.iter_result(v.unwrap_or_default(), v.is_none()).into())
    }

    fn string_next<'scope>(a: &mut Agent<'scope, '_>, this: Value<'scope>, _: &[Value<'scope>]) -> Result<Value<'scope>, Error> {
        let o = Self::state(a, this)?;
        let v = a.string_iter_step(o)?;
        Ok(a.iter_result(v.unwrap_or_default(), v.is_none()).into())
    }

    fn resume<'scope>(a: &mut Agent<'scope, '_>, this: Value<'scope>, mode: Resume, v: Value<'scope>) -> Result<Value<'scope>, Error> {
        let co = match this.kind() {
            ValueKind::Object(o) if matches!(&o.borrow().kind, ObjectKind::Coroutine(c) if c.promise.is_none() && c.requests.is_none()) => o,
            _ => return Err(a.throw(ErrorKind::Type, "Generator method called on an incompatible receiver")),
        };
        Ok(match a.resume(co, mode, v)? {
            Step::Yield(x) => return Ok(x),
            Step::Done(x) => a.iter_result(x, true),
            Step::Await(_) => unreachable!("generators do not await"),
        }
        .into())
    }

    fn generator_next<'scope>(a: &mut Agent<'scope, '_>, this: Value<'scope>, args: &[Value<'scope>]) -> Result<Value<'scope>, Error> {
        Self::resume(a, this, Resume::Next, args.at(0))
    }

    fn generator_return<'scope>(a: &mut Agent<'scope, '_>, this: Value<'scope>, args: &[Value<'scope>]) -> Result<Value<'scope>, Error> {
        Self::resume(a, this, Resume::Return, args.at(0))
    }

    fn generator_throw<'scope>(a: &mut Agent<'scope, '_>, this: Value<'scope>, args: &[Value<'scope>]) -> Result<Value<'scope>, Error> {
        Self::resume(a, this, Resume::Throw, args.at(0))
    }
}

impl Installer<'_, '_> {
    pub(crate) fn iterators(&mut self) {
        let r = self.context;
        let sym = r.symbols;
        let proto = r.symbol_proto;
        let ctor = self.constructor("Symbol", Iters::symbol, proto, 0);
        for (name, s) in sym.all() {
            self.set(ctor, name, Value::Symbol(s));
        }
        self.method(proto, "toString", Iters::symbol_to_string, 0);
        self.method(proto, "valueOf", Iters::symbol_value_of, 0);
        self.getter(proto, "description", Iters::symbol_description);
        r.symbol_method(self.mc, r.iterator_proto, sym.iterator, Iters::this_value, 0);
        let values = r.named_native(self.mc, Iters::values, "values", 0);
        self.set(r.array_proto, "values", values.into());
        r.array_proto.borrow_mut(self.mc).define_symbol(sym.iterator, j8framework::Property::hidden(values.into()));
        self.method(r.array_proto, "keys", Iters::keys, 0);
        self.method(r.array_proto, "entries", Iters::entries, 0);
        self.method(r.array_iterator_proto, "next", Iters::array_next, 0);
        r.symbol_method(self.mc, r.string_proto, sym.iterator, Iters::string_iterator, 0);
        self.method(r.string_iterator_proto, "next", Iters::string_next, 0);
        let nexts = [r.array_iterator_proto, r.string_iterator_proto].map(|o| match o.borrow().own(self.mc, "next").map(Value::kind) {
            Some(ValueKind::Object(f)) => Some(f),
            _ => None,
        });
        r.builtin_nexts.set(self.mc, nexts);
        self.method(r.generator_proto, "next", Iters::generator_next, 1);
        self.method(r.generator_proto, "return", Iters::generator_return, 1);
        self.method(r.generator_proto, "throw", Iters::generator_throw, 1);
    }
}
