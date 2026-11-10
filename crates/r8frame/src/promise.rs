//! Promise 与微任务：解决 / 拒绝、`then` 的反应、async 协程的驱动、排空队列。
use alloc::vec::Vec;

use gc_arena::{Gc, Lock};

use crate::{Agent, Error, ErrorKind, Job, JsObject, ObjectData, ObjectKind, Reaction, Resume, Settled, Step, Value};

impl<'gc> Agent<'gc, '_> {
    /// 一个待定的 Promise。
    pub fn new_promise(&self) -> JsObject<'gc> {
        ObjectData { kind: ObjectKind::Promise { state: Settled::Pending, reactions: Vec::new() }, proto: Some(self.realm.promise_proto), ..ObjectData::default() }.alloc(self.mc)
    }

    pub fn is_promise(v: Value<'gc>) -> bool {
        matches!(v, Value::Object(o) if matches!(o.borrow().kind, ObjectKind::Promise { .. }))
    }

    /// Promise 的一对 resolve / reject 函数（共享「已经解决过」的标记）。
    pub fn promise_resolvers(&self, promise: JsObject<'gc>) -> (JsObject<'gc>, JsObject<'gc>) {
        let done = Gc::new(self.mc, Lock::new(false));
        let make = |reject| ObjectData { kind: ObjectKind::Resolver { promise, reject, done }, proto: Some(self.realm.function_proto), ..ObjectData::default() }.alloc(self.mc);
        (make(false), make(true))
    }

    /// 调一个 resolve / reject 函数。
    pub fn run_resolver(&mut self, f: JsObject<'gc>, value: Value<'gc>) -> Result<Value<'gc>, Error> {
        let ObjectKind::Resolver { promise, reject, done } = f.borrow().kind else { unreachable!("a resolver function") };
        if !done.get() {
            done.set(self.mc, true);
            if reject {
                self.reject_promise(promise, value);
            } else {
                self.resolve_promise(promise, value)?;
            }
        }
        Ok(Value::Undefined)
    }

    fn enqueue(&self, job: Job<'gc>) {
        self.realm.jobs.borrow_mut(self.mc).push_back(job);
    }

    /// 落定：记下结果，把挂着的反应都排进队列。
    fn settle(&mut self, promise: JsObject<'gc>, value: Value<'gc>, rejected: bool) {
        let reactions = {
            let mut d = promise.borrow_mut(self.mc);
            let ObjectKind::Promise { state, reactions } = &mut d.kind else { return };
            if !matches!(state, Settled::Pending) {
                return;
            }
            *state = if rejected { Settled::Rejected(value) } else { Settled::Fulfilled(value) };
            core::mem::take(reactions)
        };
        for reaction in reactions {
            self.enqueue(Job::Reaction { reaction, value, rejected });
        }
    }

    pub fn reject_promise(&mut self, promise: JsObject<'gc>, reason: Value<'gc>) {
        self.settle(promise, reason, true);
    }

    /// 用一个值解决：自己解决自己抛 TypeError；thenable 排一个任务去调它的 `then`；否则直接兑现。
    pub fn resolve_promise(&mut self, promise: JsObject<'gc>, value: Value<'gc>) -> Result<(), Error> {
        if let Value::Object(o) = value {
            if Gc::ptr_eq(o, promise) {
                let e = self.error(ErrorKind::Type, "Chaining cycle detected for promise");
                self.reject_promise(promise, e.into());
                return Ok(());
            }
            let then = match self.get_named(value, "then") {
                Ok(t) => t,
                Err(Error::Exception(g)) => {
                    let reason = self.local(&g);
                    self.reject_promise(promise, reason);
                    return Ok(());
                }
                Err(e) => return Err(e),
            };
            if self.is_callable(then) {
                self.enqueue(Job::Thenable { promise, thenable: value, then });
                return Ok(());
            }
        }
        self.settle(promise, value, false);
        Ok(())
    }

    /// 挂一个反应：已经落定就直接排进队列。
    pub fn perform_then(&mut self, promise: JsObject<'gc>, reaction: Reaction<'gc>) {
        let settled = {
            let mut d = promise.borrow_mut(self.mc);
            let ObjectKind::Promise { state, reactions } = &mut d.kind else { return };
            match *state {
                Settled::Pending => {
                    reactions.push(reaction);
                    None
                }
                Settled::Fulfilled(v) => Some((v, false)),
                Settled::Rejected(v) => Some((v, true)),
            }
        };
        if let Some((value, rejected)) = settled {
            self.enqueue(Job::Reaction { reaction, value, rejected });
        }
    }

    /// `Promise.resolve(v)`：是 Promise 就原样，否则包一个。
    pub fn promise_resolve(&mut self, v: Value<'gc>) -> Result<JsObject<'gc>, Error> {
        if let Value::Object(o) = v
            && Self::is_promise(v)
        {
            return Ok(o);
        }
        let p = self.new_promise();
        self.resolve_promise(p, v)?;
        Ok(p)
    }

    /// async 协程跑一步：停在 `await` 就挂到被等的 Promise 上；跑完兑现、抛出拒绝它的结果 Promise。
    pub fn drive(&mut self, coroutine: JsObject<'gc>, mode: Resume, value: Value<'gc>) -> Result<(), Error> {
        let promise = match &coroutine.borrow().kind {
            ObjectKind::Coroutine(c) => c.promise,
            _ => None,
        };
        let Some(promise) = promise else { unreachable!("an async coroutine carries its promise") };
        match self.resume(coroutine, mode, value) {
            Ok(Step::Await(v)) => {
                let p = self.promise_resolve(v)?;
                self.perform_then(p, Reaction::Await { coroutine });
            }
            Ok(Step::Done(v)) => self.resolve_promise(promise, v)?,
            Ok(Step::Yield(_)) => unreachable!("async functions do not yield"),
            Err(Error::Exception(g)) => {
                let reason = self.local(&g);
                self.reject_promise(promise, reason);
            }
            Err(e) => return Err(e),
        }
        Ok(())
    }

    /// 做一个微任务。
    fn run_job(&mut self, job: Job<'gc>) -> Result<(), Error> {
        match job {
            Job::Reaction { reaction: Reaction::Await { coroutine }, value, rejected } => self.drive(coroutine, if rejected { Resume::Throw } else { Resume::Next }, value),
            Job::Reaction { reaction: Reaction::Then { on_fulfilled, on_rejected, derived }, value, rejected } => {
                let handler = if rejected { on_rejected } else { on_fulfilled };
                let outcome = if self.is_callable(handler) {
                    match self.call(handler, Value::Undefined, &[value]) {
                        Ok(r) => Ok(r),
                        Err(Error::Exception(g)) => Err(self.local(&g)),
                        Err(e) => return Err(e),
                    }
                } else if rejected {
                    Err(value)
                } else {
                    Ok(value)
                };
                match (derived, outcome) {
                    (Some(d), Ok(r)) => self.resolve_promise(d, r)?,
                    (Some(d), Err(reason)) => self.reject_promise(d, reason),
                    (None, _) => {}
                }
                Ok(())
            }
            Job::Reaction { reaction: Reaction::All { values, index, remaining, result }, value, rejected } => {
                if rejected {
                    self.reject_promise(result, value);
                    return Ok(());
                }
                if let ObjectKind::Array(items) = &mut values.borrow_mut(self.mc).kind
                    && let Some(slot) = items.get_mut(index as usize)
                {
                    *slot = value;
                }
                let left = remaining.get() - 1;
                remaining.set(self.mc, left);
                if left == 0 {
                    self.resolve_promise(result, values.into())?;
                }
                Ok(())
            }
            Job::Reaction { reaction: Reaction::Finally { on_finally, derived }, value, rejected } => {
                if self.is_callable(on_finally) {
                    match self.call(on_finally, Value::Undefined, &[]) {
                        Ok(_) => {}
                        Err(Error::Exception(g)) => {
                            let reason = self.local(&g);
                            self.reject_promise(derived, reason);
                            return Ok(());
                        }
                        Err(e) => return Err(e),
                    }
                }
                if rejected { self.reject_promise(derived, value) } else { self.resolve_promise(derived, value)? }
                Ok(())
            }
            Job::Thenable { promise, thenable, then } => {
                let (resolve, reject) = self.promise_resolvers(promise);
                match self.call(then, thenable, &[resolve.into(), reject.into()]) {
                    Ok(_) => Ok(()),
                    Err(Error::Exception(g)) => {
                        let reason = self.local(&g);
                        self.run_resolver(reject, reason).map(drop)
                    }
                    Err(e) => Err(e),
                }
            }
        }
    }

    /// 排空微任务队列（任务里排进来的新任务也一并做完）。
    pub fn run_jobs(&mut self) -> Result<(), Error> {
        loop {
            let job = self.realm.jobs.borrow_mut(self.mc).pop_front();
            match job {
                Some(j) => self.run_job(j)?,
                None => return Ok(()),
            }
        }
    }
}
