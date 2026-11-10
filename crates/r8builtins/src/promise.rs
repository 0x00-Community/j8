use alloc::vec::Vec;

use gc_arena::{Gc, Lock};
use r8frame::{Agent, Error, ErrorKind, JsObject, ObjectKind, Reaction, Value};

use crate::{Args, Installer};

/// `Promise` 与原型方法。
struct Promises;

impl Promises {
    fn this<'gc>(a: &Agent<'gc, '_>, this: Value<'gc>) -> Result<JsObject<'gc>, Error> {
        match this {
            Value::Object(o) if matches!(o.borrow().kind, ObjectKind::Promise { .. }) => Ok(o),
            _ => Err(a.throw(ErrorKind::Type, "Promise.prototype method called on a non-promise")),
        }
    }

    /// `new Promise(executor)`：同步调 `executor(resolve, reject)`，它抛错就拒绝。
    fn construct<'gc>(a: &mut Agent<'gc, '_>, _: Value<'gc>, args: &[Value<'gc>]) -> Result<Value<'gc>, Error> {
        let executor = args.at(0);
        if !a.is_callable(executor) {
            return Err(a.throw(ErrorKind::Type, "Promise resolver is not a function"));
        }
        let p = a.new_promise();
        let (resolve, reject) = a.promise_resolvers(p);
        match a.call(executor, Value::Undefined, &[resolve.into(), reject.into()]) {
            Ok(_) => {}
            Err(Error::Exception(g)) => {
                let reason = a.local(&g);
                a.run_resolver(reject, reason)?;
            }
            Err(e) => return Err(e),
        }
        Ok(p.into())
    }

    fn resolve<'gc>(a: &mut Agent<'gc, '_>, _: Value<'gc>, args: &[Value<'gc>]) -> Result<Value<'gc>, Error> {
        Ok(a.promise_resolve(args.at(0))?.into())
    }

    fn reject<'gc>(a: &mut Agent<'gc, '_>, _: Value<'gc>, args: &[Value<'gc>]) -> Result<Value<'gc>, Error> {
        let p = a.new_promise();
        a.reject_promise(p, args.at(0));
        Ok(p.into())
    }

    /// `Promise.all(iterable)`。
    fn all<'gc>(a: &mut Agent<'gc, '_>, this: Value<'gc>, args: &[Value<'gc>]) -> Result<Value<'gc>, Error> {
        Self::combine(a, this, args.at(0), false)
    }

    /// `Promise.race(iterable)`：谁先落定用谁。
    fn race<'gc>(a: &mut Agent<'gc, '_>, this: Value<'gc>, args: &[Value<'gc>]) -> Result<Value<'gc>, Error> {
        Self::combine(a, this, args.at(0), true)
    }

    /// `all` / `race` 共用：出错（迭代器、`C.resolve`、`then`）都变成拒绝结果 Promise，不往外抛。
    fn combine<'gc>(a: &mut Agent<'gc, '_>, ctor: Value<'gc>, iterable: Value<'gc>, race: bool) -> Result<Value<'gc>, Error> {
        if !matches!(ctor, Value::Object(_)) {
            return Err(a.throw(ErrorKind::Type, "Promise.all / race called on a non-object"));
        }
        let result = a.new_promise();
        match Self::feed(a, ctor, iterable, result, race) {
            Ok(()) => {}
            Err(Error::Exception(g)) => {
                let reason = a.local(&g);
                a.reject_promise(result, reason);
            }
            Err(e) => return Err(e),
        }
        Ok(result.into())
    }

    /// 逐个从迭代器取值（不先取完：迭代器可以是无穷的），`C.resolve(v)` 后挂到结果上。
    /// 迭代器自己出错不关它；`resolve` / `then` 出错先关迭代器（关的时候再出错以原错误为准）。
    fn feed<'gc>(a: &mut Agent<'gc, '_>, ctor: Value<'gc>, iterable: Value<'gc>, result: JsObject<'gc>, race: bool) -> Result<(), Error> {
        let it = a.get_iterator(iterable)?;
        let resolve = match a.get_named(ctor, "resolve") {
            Ok(f) if a.is_callable(f) => f,
            Ok(_) => {
                let e = a.throw(ErrorKind::Type, "Promise resolve is not a function");
                return Self::close(a, it.iterator, e);
            }
            Err(e) => return Self::close(a, it.iterator, e),
        };
        let values = a.array(Vec::new());
        // 比元素数多 1：取完才减掉这一个，免得中途全兑现就提前落定
        let remaining = Gc::new(a.mc, Lock::new(1u32));
        let (resolve_result, reject_result) = a.promise_resolvers(result);
        let mut index = 0u32;
        while let Some(v) = a.iterator_step(it)? {
            let step = Self::item(a, ctor, resolve, v, race.then_some(resolve_result), reject_result, Reaction::All { values, index, remaining, result });
            if let Err(e) = step {
                return Self::close(a, it.iterator, e);
            }
            index += 1;
        }
        if !race {
            let left = remaining.get() - 1;
            remaining.set(a.mc, left);
            if left == 0 {
                a.resolve_promise(result, values.into())?;
            }
        }
        Ok(())
    }

    /// 一项：`next = C.resolve(v)`，再 `next.then(…)`。`race` 给 `Some(结果的 resolve)`；`all` 用 `all` 反应填 `values`。
    fn item<'gc>(a: &mut Agent<'gc, '_>, ctor: Value<'gc>, resolve: Value<'gc>, v: Value<'gc>, race: Option<JsObject<'gc>>, reject: JsObject<'gc>, all: Reaction<'gc>) -> Result<(), Error> {
        let next = a.call(resolve, ctor, &[v])?;
        if let Reaction::All { values, index, remaining, .. } = all {
            values.borrow_mut(a.mc).write_index(a.mc, index, Value::Undefined);
            remaining.set(a.mc, remaining.get() + 1);
        }
        let then = a.get_named(next, "then")?;
        let intrinsic = matches!(then, Value::Object(f) if matches!(f.borrow().kind, ObjectKind::Native(n) if core::ptr::fn_addr_eq(n.0, Self::then as r8frame::NativeFn)));
        match (next, intrinsic, race) {
            // 原生 Promise 配原生 `then`：直接挂内部反应
            (Value::Object(p), true, Some(_)) if Self::this(a, next).is_ok() => {
                let Reaction::All { result, .. } = all else { unreachable!("feed passes an all reaction") };
                a.perform_then(p, Reaction::Then { on_fulfilled: Value::Undefined, on_rejected: Value::Undefined, derived: Some(result) });
            }
            (Value::Object(p), true, None) if Self::this(a, next).is_ok() => a.perform_then(p, all),
            // 别的 `then`：照规范调它；`all` 的元素经一个中间 Promise 接回 `all` 反应
            (_, _, Some(resolve_result)) => {
                a.call(then, next, &[resolve_result.into(), reject.into()])?;
            }
            (_, _, None) => {
                let element = a.new_promise();
                let (on_fulfilled, _) = a.promise_resolvers(element);
                a.perform_then(element, all);
                a.call(then, next, &[on_fulfilled.into(), reject.into()])?;
            }
        }
        Ok(())
    }

    /// IteratorClose 后把原错误交回去（关闭本身出的错丢掉）。
    fn close<'gc>(a: &mut Agent<'gc, '_>, iterator: Value<'gc>, e: Error) -> Result<(), Error> {
        let _ = a.iterator_close(iterator);
        Err(e)
    }

    fn then<'gc>(a: &mut Agent<'gc, '_>, this: Value<'gc>, args: &[Value<'gc>]) -> Result<Value<'gc>, Error> {
        let p = Self::this(a, this)?;
        let derived = a.new_promise();
        a.perform_then(p, Reaction::Then { on_fulfilled: args.at(0), on_rejected: args.at(1), derived: Some(derived) });
        Ok(derived.into())
    }

    fn catch<'gc>(a: &mut Agent<'gc, '_>, this: Value<'gc>, args: &[Value<'gc>]) -> Result<Value<'gc>, Error> {
        Self::then(a, this, &[Value::Undefined, args.at(0)])
    }

    fn finally<'gc>(a: &mut Agent<'gc, '_>, this: Value<'gc>, args: &[Value<'gc>]) -> Result<Value<'gc>, Error> {
        let p = Self::this(a, this)?;
        let derived = a.new_promise();
        a.perform_then(p, Reaction::Finally { on_finally: args.at(0), derived });
        Ok(derived.into())
    }
}

impl Installer<'_, '_> {
    pub(crate) fn promise(&mut self) {
        let proto = self.realm.promise_proto;
        let ctor = self.constructor("Promise", Promises::construct, proto);
        self.method(ctor, "resolve", Promises::resolve);
        self.method(ctor, "reject", Promises::reject);
        self.method(ctor, "all", Promises::all);
        self.method(ctor, "race", Promises::race);
        self.method(proto, "then", Promises::then);
        self.method(proto, "catch", Promises::catch);
        self.method(proto, "finally", Promises::finally);
    }
}
