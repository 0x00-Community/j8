use r8frame::{Agent, Error, ErrorKind, ObjectKind, Resume, Value};

use crate::{Args, Installer};

/// %AsyncGeneratorPrototype% 与 %AsyncIteratorPrototype%。
struct AsyncGenerators;

impl AsyncGenerators {
    /// `next` / `return` / `throw`：排进请求队列，给出这个请求的 Promise（`this` 不对给一个拒绝的 Promise）。
    fn enqueue<'gc>(a: &mut Agent<'gc, '_>, this: Value<'gc>, mode: Resume, v: Value<'gc>) -> Result<Value<'gc>, Error> {
        let co = match this {
            Value::Object(o) if matches!(&o.borrow().kind, ObjectKind::Coroutine(c) if c.requests.is_some()) => o,
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
        r.symbol_method(self.mc, r.async_iterator_proto, r.symbols.async_iterator, AsyncGenerators::this_value);
        let p = r.async_generator_proto;
        self.method(p, "next", AsyncGenerators::next);
        self.method(p, "return", AsyncGenerators::return_);
        self.method(p, "throw", AsyncGenerators::throw);
    }
}
