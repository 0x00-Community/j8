use r8framework::ValueKind;
use r8framework::{Agent, Error, ErrorKind, ObjectKind, Resume, Value};

use crate::{Args, Installer};

/// %AsyncGeneratorPrototype% and %AsyncIteratorPrototype%.
struct AsyncGenerators;

impl AsyncGenerators {
    /// `next` / `return` / `throw`: enqueue the request and return its Promise (a rejected Promise if `this` is wrong).
    fn enqueue<'gc>(a: &mut Agent<'gc, '_>, this: Value<'gc>, mode: Resume, v: Value<'gc>) -> Result<Value<'gc>, Error> {
        let co = match this.kind() {
            ValueKind::Object(o) if matches!(&o.borrow().kind, ObjectKind::Coroutine(c) if c.requests.is_some()) => o,
            _ => {
                let p = a.new_promise();
                let e = a.error(ErrorKind::Type, "AsyncGenerator method called on an incompatible receiver");
                a.reject_promise(p, e.into());
                return Ok(p.into());
            }
        };
        Ok(a.async_generator_enqueue(co, mode, v)?.into())
    }

    fn next<'gc>(a: &mut Agent<'gc, '_>, this: Value<'gc>, args: &[Value<'gc>]) -> Result<Value<'gc>, Error> {
        Self::enqueue(a, this, Resume::Next, args.at(0))
    }

    fn return_<'gc>(a: &mut Agent<'gc, '_>, this: Value<'gc>, args: &[Value<'gc>]) -> Result<Value<'gc>, Error> {
        Self::enqueue(a, this, Resume::Return, args.at(0))
    }

    fn throw<'gc>(a: &mut Agent<'gc, '_>, this: Value<'gc>, args: &[Value<'gc>]) -> Result<Value<'gc>, Error> {
        Self::enqueue(a, this, Resume::Throw, args.at(0))
    }

    fn this_value<'gc>(_: &mut Agent<'gc, '_>, this: Value<'gc>, _: &[Value<'gc>]) -> Result<Value<'gc>, Error> {
        Ok(this)
    }
}

impl Installer<'_, '_> {
    pub(crate) fn async_generators(&mut self) {
        let r = self.realm;
        r.symbol_method(self.mc, r.async_iterator_proto, r.symbols.async_iterator, AsyncGenerators::this_value, 0);
        let p = r.async_generator_proto;
        self.method(p, "next", AsyncGenerators::next, 1);
        self.method(p, "return", AsyncGenerators::return_, 1);
        self.method(p, "throw", AsyncGenerators::throw, 1);
    }
}
