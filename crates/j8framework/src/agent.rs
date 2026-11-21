use crate::ValueKind;
use alloc::vec::Vec;

use crate::{Collect, Mutation};

use crate::{Context, Error, JsObject, Platform, Profiler, Scope, Value};

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum CodeKind {
    Eval,
    StrictEval,
    Script,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Collect)]
#[collect(require_static)]
pub enum Resume {
    Next,
    Throw,
    Return,
}

impl Resume {
    pub const ALL: [Resume; 3] = [Resume::Next, Resume::Throw, Resume::Return];
}

impl From<Resume> for f64 {
    fn from(m: Resume) -> f64 {
        match m {
            Resume::Next => 0.0,
            Resume::Return => 1.0,
            Resume::Throw => 2.0,
        }
    }
}

#[derive(Clone, Copy, Debug)]
pub enum Step<'gc> {
    Yield(Value<'gc>),
    Await(Value<'gc>),
    Done(Value<'gc>),
}

pub trait Invoke<'gc> {
    fn call(&self, agent: &mut Agent<'gc, '_>, callee: Value<'gc>, this: Value<'gc>, args: &[Value<'gc>]) -> Result<Value<'gc>, Error>;

    fn construct(&self, agent: &mut Agent<'gc, '_>, callee: Value<'gc>, args: &[Value<'gc>], new_target: Value<'gc>) -> Result<Value<'gc>, Error>;

    fn eval_code(&self, agent: &mut Agent<'gc, '_>, src: &str, kind: CodeKind) -> Result<Value<'gc>, Error>;
    fn constructs(&self, closure: &crate::Closure<'gc>) -> bool;

    fn resume(&self, agent: &mut Agent<'gc, '_>, coroutine: crate::JsObject<'gc>, mode: Resume, value: Value<'gc>) -> Result<Step<'gc>, Error>;

    fn callback(&self, _agent: &mut Agent<'gc, '_>, callee: Value<'gc>, this: Value<'gc>) -> Callback<'gc> {
        Callback::from((callee, this))
    }
}

pub type CallbackRun = for<'gc, 'x> unsafe fn(&mut Agent<'gc, 'x>, &Callback<'gc>, &[Value<'gc>]) -> Option<Result<Value<'gc>, Error>>;

#[derive(Clone, Copy)]
pub struct Callback<'gc> {
    pub callee: Value<'gc>,
    pub this: Value<'gc>,
    pub run: Option<CallbackRun>,
    pub code: usize,
    pub arity: usize,
    pub env: [usize; 2],
}

impl<'gc> From<(Value<'gc>, Value<'gc>)> for Callback<'gc> {
    fn from((callee, this): (Value<'gc>, Value<'gc>)) -> Self {
        Self { callee, this, run: None, code: 0, arity: 0, env: [0; 2] }
    }
}

impl<'gc> Callback<'gc> {
    #[inline]
    pub fn call(&self, a: &mut Agent<'gc, '_>, args: &[Value<'gc>]) -> Result<Value<'gc>, Error> {
        if let Some(run) = self.run
            && let Some(r) = unsafe { run(a, self, args) }
        {
            return r;
        }
        let invoke = a.invoke;
        invoke.call(a, self.callee, self.this, args)
    }
}

pub struct Agent<'gc, 'x> {
    pub mc: &'x Mutation<'gc>,
    pub platform: &'x dyn Platform,
    pub profiler: &'x mut Profiler,
    pub realm: Context<'gc>,
    pub new_target: Value<'gc>,
    invoke: &'x dyn Invoke<'gc>,
    spawned: Option<&'x j8lockfree::Fifo<crate::AsyncJob>>,
    pub stop: &'x crate::Termination,
    spare_regs: Vec<Vec<Value<'gc>>>,
}

impl<'gc> Scope<'gc, '_> {
    pub fn agent<'x>(&'x mut self, realm: Context<'gc>, invoke: &'x dyn Invoke<'gc>) -> Agent<'gc, 'x> {
        Agent { mc: self.mc, platform: self.platform, profiler: &mut *self.profiler, realm, new_target: Value::Undefined, invoke, spawned: self.spawned, stop: self.stop, spare_regs: Vec::new() }
    }
}

impl<'gc> Agent<'gc, '_> {
    const SPARE_REGS: usize = 8;

    pub fn spawn(&self, job: crate::AsyncJob) -> Result<(), crate::AsyncJob> {
        match self.spawned {
            Some(q) => {
                q.push(job);
                Ok(())
            }
            None => Err(job),
        }
    }

    pub fn take_regs(&mut self) -> Vec<Value<'gc>> {
        self.spare_regs.pop().unwrap_or_default()
    }

    pub fn give_regs(&mut self, mut regs: Vec<Value<'gc>>) {
        if self.spare_regs.len() < Self::SPARE_REGS {
            regs.clear();
            self.spare_regs.push(regs);
        }
    }
}

impl<'gc> Agent<'gc, '_> {
    pub fn call(&mut self, callee: Value<'gc>, this: Value<'gc>, args: &[Value<'gc>]) -> Result<Value<'gc>, Error> {
        let invoke = self.invoke;
        invoke.call(self, callee, this, args)
    }

    pub fn callback(&mut self, callee: Value<'gc>, this: Value<'gc>) -> Callback<'gc> {
        let invoke = self.invoke;
        invoke.callback(self, callee, this)
    }

    pub fn is_constructor(&self, v: Value<'gc>) -> bool {
        let Some(o) = v.as_object() else { return false };
        match &o.borrow().kind {
            crate::ObjectKind::Function(c) => self.invoke.constructs(c),
            crate::ObjectKind::Native(n) => n.constructor,
            crate::ObjectKind::NativeClosure(c) => c.constructor,
            crate::ObjectKind::Bound(b) => self.is_constructor(b.target),
            crate::ObjectKind::Proxy(p) => p.constructor,
            _ => false,
        }
    }

    pub fn proto_from_ctor(&mut self, new_target: Value<'gc>, fallback: JsObject<'gc>) -> Result<JsObject<'gc>, Error> {
        if new_target == Value::Undefined {
            return Ok(fallback);
        }
        match (self.get_named(new_target, "prototype")?).kind() {
            ValueKind::Object(p) => Ok(p),
            _ => Ok(fallback),
        }
    }

    pub fn create_from_ctor(&mut self, new_target: Value<'gc>, fallback: JsObject<'gc>, kind: crate::ObjectKind<'gc>) -> Result<JsObject<'gc>, Error> {
        let proto = self.proto_from_ctor(new_target, fallback)?;
        Ok(crate::ObjectData { kind, proto: Some(proto), ..crate::ObjectData::default() }.alloc(self.mc))
    }

    pub fn wrap_primitive(&mut self, new_target: Value<'gc>, v: Value<'gc>, fallback: JsObject<'gc>) -> Result<Value<'gc>, Error> {
        if new_target == Value::Undefined {
            return Ok(v);
        }
        Ok(self.create_from_ctor(new_target, fallback, crate::ObjectKind::Primitive(v))?.into())
    }

    pub fn eval_code(&mut self, src: &str, kind: CodeKind) -> Result<Value<'gc>, Error> {
        let invoke = self.invoke;
        invoke.eval_code(self, src, kind)
    }

    pub fn resume(&mut self, coroutine: crate::JsObject<'gc>, mode: Resume, value: Value<'gc>) -> Result<Step<'gc>, Error> {
        let invoke = self.invoke;
        invoke.resume(self, coroutine, mode, value)
    }

    pub fn construct(&mut self, callee: Value<'gc>, args: &[Value<'gc>], new_target: Value<'gc>) -> Result<Value<'gc>, Error> {
        let invoke = self.invoke;
        invoke.construct(self, callee, args, new_target)
    }
}
