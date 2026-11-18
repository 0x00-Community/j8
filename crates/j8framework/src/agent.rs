use crate::ValueKind;
use alloc::vec::Vec;

use crate::{Collect, Mutation};

use crate::{Context, Error, JsObject, Platform, Profiler, Scope, Value};

/// The kind of dynamic code.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum CodeKind {
    /// Indirect `eval` / `Function` constructor: top-level `let` / `const` stay within this code
    Eval,
    /// Direct `eval` in strict code: the code itself is strict, and `var` and function declarations also stay within it
    StrictEval,
    /// A new script (`$262.evalScript`): top-level lexical declarations go into the script scope
    Script,
}

/// How to resume a coroutine.
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

/// V8's `JSGeneratorObject::ResumeMode` (what `GeneratorGetResumeMode` gives the bytecode).
impl From<Resume> for f64 {
    fn from(m: Resume) -> f64 {
        match m {
            Resume::Next => 0.0,
            Resume::Return => 1.0,
            Resume::Throw => 2.0,
        }
    }
}

/// The result of running a coroutine one step.
#[derive(Clone, Copy, Debug)]
pub enum Step<'gc> {
    /// Stopped at `yield`: what the generator gives its caller (for a sync generator, the iterator result itself)
    Yield(Value<'gc>),
    /// Stopped at `await`
    Await(Value<'gc>),
    /// Finished (the return value)
    Done(Value<'gc>),
}

/// Callbacks provided by the execution engine: operations (accessors, ToPrimitive, host functions) go through it to
/// call back into JS.
pub trait Invoke<'gc> {
    /// Call a value; throws a TypeError if it is not callable.
    fn call(&self, agent: &mut Agent<'gc, '_>, callee: Value<'gc>, this: Value<'gc>, args: &[Value<'gc>]) -> Result<Value<'gc>, Error>;

    /// Construct call (`new callee(...args)`, with `new.target` being `new_target`); throws a TypeError if it is not
    /// constructible.
    fn construct(&self, agent: &mut Agent<'gc, '_>, callee: Value<'gc>, args: &[Value<'gc>], new_target: Value<'gc>) -> Result<Value<'gc>, Error>;

    /// Compile and run a piece of code in the current context, returning the completion value; a syntax error throws a
    /// SyntaxError, and an engine without a compiler also throws a SyntaxError.
    fn eval_code(&self, agent: &mut Agent<'gc, '_>, src: &str, kind: CodeKind) -> Result<Value<'gc>, Error>;
    /// Whether this closure can be `new`ed (ordinary functions and class constructors can; arrow functions, methods,
    /// generators, and async functions cannot).
    fn constructs(&self, closure: &crate::Closure<'gc>) -> bool;

    /// Resume a coroutine (`ObjectKind::Coroutine`) for one step: `Next` uses `value` as the result of `yield` /
    /// `await`, `Throw` throws at the suspension point, `Return` returns at the suspension point (running finally).
    fn resume(&self, agent: &mut Agent<'gc, '_>, coroutine: crate::JsObject<'gc>, mode: Resume, value: Value<'gc>) -> Result<Step<'gc>, Error>;
}

/// The whole context of one run: GC heap, platform, profiler, context, callbacks. All JS operation semantics hang off
/// it.
pub struct Agent<'gc, 'x> {
    pub mc: &'x Mutation<'gc>,
    pub platform: &'x dyn Platform,
    pub profiler: &'x mut Profiler,
    pub realm: Context<'gc>,
    /// `new.target` when the running host function is a construct call, `undefined` for a plain call (set by the
    /// executor before calling the host function)
    pub new_target: Value<'gc>,
    invoke: &'x dyn Invoke<'gc>,
    /// The isolate's queue of async host work (`None` without an event loop)
    spawned: Option<&'x j8lockfree::Fifo<crate::AsyncJob>>,
    /// 中止请求：执行层在检查点读它
    pub stop: &'x crate::Termination,
    /// Empty register files returned after use (each host function callback into JS needs one; reuse saves the
    /// allocation)
    spare_regs: Vec<Vec<Value<'gc>>>,
}

impl<'gc> Scope<'gc, '_> {
    /// Lend out an agent in context `realm`, calling back into JS via `invoke`.
    pub fn agent<'x>(&'x mut self, realm: Context<'gc>, invoke: &'x dyn Invoke<'gc>) -> Agent<'gc, 'x> {
        Agent { mc: self.mc, platform: self.platform, profiler: &mut *self.profiler, realm, new_target: Value::Undefined, invoke, spawned: self.spawned, stop: self.stop, spare_regs: Vec::new() }
    }
}

impl<'gc> Agent<'gc, '_> {
    /// At most how many register files to keep for reuse
    const SPARE_REGS: usize = 8;

    /// Hands async host work to this isolate's event loop (`Err` gives it back when the isolate runs none, see
    /// [`CreateParams::event_loop`](crate::CreateParams::event_loop)).
    pub fn spawn(&self, job: crate::AsyncJob) -> Result<(), crate::AsyncJob> {
        match self.spawned {
            Some(q) => {
                q.push(job);
                Ok(())
            }
            None => Err(job),
        }
    }

    /// Borrow an empty register file (used by the executor to start a new execution state)
    pub fn take_regs(&mut self) -> Vec<Value<'gc>> {
        self.spare_regs.pop().unwrap_or_default()
    }

    /// Return a register file (cleared and kept for next time)
    pub fn give_regs(&mut self, mut regs: Vec<Value<'gc>>) {
        if self.spare_regs.len() < Self::SPARE_REGS {
            regs.clear();
            self.spare_regs.push(regs);
        }
    }
}

impl<'gc> Agent<'gc, '_> {
    /// Call a JS value.
    pub fn call(&mut self, callee: Value<'gc>, this: Value<'gc>, args: &[Value<'gc>]) -> Result<Value<'gc>, Error> {
        let invoke = self.invoke;
        invoke.call(self, callee, this, args)
    }

    /// IsConstructor.
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

    /// GetPrototypeFromConstructor: use `new_target.prototype` if it is an object, otherwise `fallback`.
    pub fn proto_from_ctor(&mut self, new_target: Value<'gc>, fallback: JsObject<'gc>) -> Result<JsObject<'gc>, Error> {
        if new_target == Value::Undefined {
            return Ok(fallback);
        }
        match (self.get_named(new_target, "prototype")?).kind() {
            ValueKind::Object(p) => Ok(p),
            _ => Ok(fallback),
        }
    }

    /// OrdinaryCreateFromConstructor: the prototype comes from `new_target` (`fallback` for a plain call, where it is
    /// `undefined`).
    pub fn create_from_ctor(&mut self, new_target: Value<'gc>, fallback: JsObject<'gc>, kind: crate::ObjectKind<'gc>) -> Result<JsObject<'gc>, Error> {
        let proto = self.proto_from_ctor(new_target, fallback)?;
        Ok(crate::ObjectData { kind, proto: Some(proto), ..crate::ObjectData::default() }.alloc(self.mc))
    }

    /// The tail of the Boolean / Number / String constructors: a plain call returns the primitive, a construct call
    /// wraps it in a wrapper object.
    pub fn wrap_primitive(&mut self, new_target: Value<'gc>, v: Value<'gc>, fallback: JsObject<'gc>) -> Result<Value<'gc>, Error> {
        if new_target == Value::Undefined {
            return Ok(v);
        }
        Ok(self.create_from_ctor(new_target, fallback, crate::ObjectKind::Primitive(v))?.into())
    }

    /// Compile and run a piece of dynamic code.
    pub fn eval_code(&mut self, src: &str, kind: CodeKind) -> Result<Value<'gc>, Error> {
        let invoke = self.invoke;
        invoke.eval_code(self, src, kind)
    }

    /// Resume a coroutine for one step.
    pub fn resume(&mut self, coroutine: crate::JsObject<'gc>, mode: Resume, value: Value<'gc>) -> Result<Step<'gc>, Error> {
        let invoke = self.invoke;
        invoke.resume(self, coroutine, mode, value)
    }

    /// Construct call.
    pub fn construct(&mut self, callee: Value<'gc>, args: &[Value<'gc>], new_target: Value<'gc>) -> Result<Value<'gc>, Error> {
        let invoke = self.invoke;
        invoke.construct(self, callee, args, new_target)
    }
}
