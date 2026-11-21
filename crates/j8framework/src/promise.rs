use crate::ValueKind;
use alloc::vec::Vec;

use crate::Gc;

use crate::{Agent, Error, ErrorKind, HostReaction, Job, JsObject, ObjectData, ObjectKind, Reaction, Request, Resume, Settled, Static, Step, Value};

impl<'gc> Agent<'gc, '_> {
    pub fn new_promise(&self) -> JsObject<'gc> {
        ObjectData { kind: ObjectKind::Promise { state: Settled::Pending, reactions: Vec::new() }, proto: Some(self.realm.promise_proto), ..ObjectData::default() }.alloc(self.mc)
    }

    pub fn is_promise(v: Value<'gc>) -> bool {
        matches!(v.kind(), ValueKind::Object(o) if matches!(o.borrow().kind, ObjectKind::Promise { .. }))
    }

    pub fn promise_resolvers(&self, promise: JsObject<'gc>) -> (JsObject<'gc>, JsObject<'gc>) {
        let done = self.mc.alloc(core::sync::atomic::AtomicBool::new(false));
        let make = |reject| {
            let f = ObjectData { kind: ObjectKind::Resolver { promise, reject, done }, proto: Some(self.realm.function_proto), ..ObjectData::default() }.alloc(self.mc);
            self.realm.name_function(self.mc, f, "", Some(1));
            f
        };
        (make(false), make(true))
    }

    pub fn run_resolver(&mut self, f: JsObject<'gc>, value: Value<'gc>) -> Result<Value<'gc>, Error> {
        let ObjectKind::Resolver { promise, reject, done } = f.borrow().kind else { unreachable!("a resolver function") };
        if !done.swap(true, core::sync::atomic::Ordering::AcqRel) {
            if reject {
                self.reject_promise(promise, value);
            } else {
                self.resolve_promise(promise, value)?;
            }
        }
        Ok(Value::Undefined)
    }

    fn enqueue(&self, job: Job<'gc>) {
        self.realm.jobs.push(self.mc, job);
    }

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

    pub fn resolve_promise(&mut self, promise: JsObject<'gc>, value: Value<'gc>) -> Result<(), Error> {
        if let Some(o) = value.as_object() {
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

    pub fn on_settled(&mut self, v: Value<'gc>, reaction: HostReaction) -> Result<(), Error> {
        let p = self.promise_resolve(v)?;
        let hook = self.mc.alloc(Static(j8lockfree::AtomicRefCell::new(Some(reaction))));
        self.perform_then(p, Reaction::Host { hook });
        Ok(())
    }

    pub fn promise_resolve(&mut self, v: Value<'gc>) -> Result<JsObject<'gc>, Error> {
        if let Some(o) = v.as_object()
            && Self::is_promise(v)
            && matches!((self.get_named(v, "constructor")?).kind(), ValueKind::Object(c) if Gc::ptr_eq(c, self.realm.promise_ctor))
        {
            return Ok(o);
        }
        let p = self.new_promise();
        self.resolve_promise(p, v)?;
        Ok(p)
    }

    pub fn drive(&mut self, coroutine: JsObject<'gc>, mode: Resume, value: Value<'gc>) -> Result<(), Error> {
        let (promise, async_generator) = match &coroutine.borrow().kind {
            ObjectKind::Coroutine(c) => (c.promise, c.requests.is_some()),
            _ => (None, false),
        };
        if async_generator {
            return self.drive_async_generator(coroutine, mode, value);
        }
        match self.resume(coroutine, mode, value) {
            Ok(_) => Ok(()),
            Err(Error::Exception(g)) => {
                let reason = self.local(&g);
                if let Some(p) = promise {
                    self.reject_promise(p, reason);
                }
                Ok(())
            }
            Err(e) => Err(e),
        }
    }

    pub fn async_generator_enqueue(&mut self, coroutine: JsObject<'gc>, mode: Resume, value: Value<'gc>) -> Result<JsObject<'gc>, Error> {
        let promise = self.new_promise();
        let idle = {
            let mut d = coroutine.borrow_mut(self.mc);
            let ObjectKind::Coroutine(c) = &mut d.kind else { unreachable!("an async generator") };
            let Some(q) = &mut c.requests else { unreachable!("an async generator") };
            q.push_back(Request { mode, value, promise });
            q.len() == 1
        };
        if idle {
            self.drive_async_generator(coroutine, mode, value)?;
        }
        Ok(promise)
    }

    fn settle_request(&mut self, coroutine: JsObject<'gc>, outcome: Result<(Value<'gc>, bool), Value<'gc>>) -> Result<(), Error> {
        let (done_req, next) = {
            let mut d = coroutine.borrow_mut(self.mc);
            let ObjectKind::Coroutine(c) = &mut d.kind else { unreachable!("an async generator") };
            let Some(q) = &mut c.requests else { unreachable!("an async generator") };
            (q.pop_front(), q.front().copied())
        };
        if let Some(req) = done_req {
            match outcome {
                Ok((v, done)) => {
                    let r = self.iter_result(v, done);
                    self.resolve_promise(req.promise, r.into())?;
                }
                Err(reason) => self.reject_promise(req.promise, reason),
            }
        }
        match next {
            Some(n) => self.drive_async_generator(coroutine, n.mode, n.value),
            None => Ok(()),
        }
    }

    fn drive_async_generator(&mut self, coroutine: JsObject<'gc>, mode: Resume, value: Value<'gc>) -> Result<(), Error> {
        match self.resume(coroutine, mode, value) {
            Ok(Step::Await(v)) => match self.promise_resolve(v) {
                Ok(p) => {
                    self.perform_then(p, Reaction::Await { coroutine });
                    Ok(())
                }
                Err(Error::Exception(g)) => {
                    let reason = self.local(&g);
                    self.drive_async_generator(coroutine, Resume::Throw, reason)
                }
                Err(e) => Err(e),
            },
            Ok(Step::Yield(v)) => self.settle_request(coroutine, Ok((v, false))),
            Ok(Step::Done(v)) => self.settle_request(coroutine, Ok((v, true))),
            Err(Error::Exception(g)) => {
                let reason = self.local(&g);
                self.settle_request(coroutine, Err(reason))
            }
            Err(e) => Err(e),
        }
    }

    fn run_job(&mut self, job: Job<'gc>) -> Result<(), Error> {
        match job {
            Job::Reaction { reaction: Reaction::Await { coroutine }, value, rejected } => self.drive(coroutine, if rejected { Resume::Throw } else { Resume::Next }, value),
            Job::Reaction { reaction: Reaction::Host { hook }, value, rejected } => {
                if let Some(reaction) = hook.0.try_borrow_mut().and_then(|mut h| h.take()) {
                    (reaction.0)(self, value, rejected);
                }
                Ok(())
            }
            Job::Reaction { reaction: Reaction::Then { on_fulfilled, on_rejected, capability }, value, rejected } => {
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
                if let Some(cap) = capability {
                    let r = match outcome {
                        Ok(r) => self.call(cap.resolve, Value::Undefined, &[r]),
                        Err(reason) => self.call(cap.reject, Value::Undefined, &[reason]),
                    };
                    match r {
                        Ok(_) | Err(Error::Exception(_)) => {}
                        Err(e) => return Err(e),
                    }
                }
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

    pub fn run_jobs(&mut self) -> Result<(), Error> {
        loop {
            let job = self.realm.jobs.pop(self.mc);
            match job {
                Some(j) => self.run_job(j)?,
                None => return Ok(()),
            }
        }
    }
}
