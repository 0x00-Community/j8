use gc_arena::{Collect, DynamicRootSet, Mutation};

use crate::{Context, Error, Platform, Profiler, Scope, Value};

/// 恢复协程的方式。
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

/// 恢复方式写进寄存器时的数（编译器在 `yield` 之后按它分派）。
impl From<Resume> for f64 {
    fn from(m: Resume) -> f64 {
        match m {
            Resume::Next => 0.0,
            Resume::Throw => 1.0,
            Resume::Return => 2.0,
        }
    }
}

/// 协程跑一步的结果。
#[derive(Clone, Copy, Debug)]
pub enum Step<'gc> {
    /// 停在 `yield`
    Yield(Value<'gc>),
    /// 停在 `await`
    Await(Value<'gc>),
    /// 跑完了（返回值）
    Done(Value<'gc>),
}

/// 执行引擎提供的回调：运算（访问器、ToPrimitive、宿主函数）要调回 JS 时经它。
pub trait Invoke<'gc> {
    /// 调一个值；不可调就抛 TypeError。
    fn call(&self, agent: &mut Agent<'gc, '_>, callee: Value<'gc>, this: Value<'gc>, args: &[Value<'gc>]) -> Result<Value<'gc>, Error>;

    /// 构造调用（`new callee(...args)`，`new.target` 是 `new_target`）；不可构造就抛 TypeError。
    fn construct(&self, agent: &mut Agent<'gc, '_>, callee: Value<'gc>, args: &[Value<'gc>], new_target: Value<'gc>) -> Result<Value<'gc>, Error>;

    /// 恢复一个协程（`ObjectKind::Coroutine`）跑一步：`Next` 把 `value` 当 `yield` / `await` 的结果，
    /// `Throw` 在挂起处抛出，`Return` 在挂起处返回（经过 finally）。
    fn resume(&self, agent: &mut Agent<'gc, '_>, coroutine: crate::JsObject<'gc>, mode: Resume, value: Value<'gc>) -> Result<Step<'gc>, Error>;
}

/// 一次运行的全部上下文：GC 堆、平台、剖析器、执行环境、回调。JS 的运算语义都挂在它上面。
pub struct Agent<'gc, 'x> {
    pub mc: &'gc Mutation<'gc>,
    pub(crate) roots: &'gc DynamicRootSet<'gc>,
    pub platform: &'x dyn Platform,
    pub profiler: &'x mut Profiler,
    pub realm: Context<'gc>,
    invoke: &'x dyn Invoke<'gc>,
}

impl<'gc> Scope<'gc, '_> {
    /// 在执行环境 `realm` 里、以 `invoke` 回调 JS，借出一个 agent。
    pub fn agent<'x>(&'x mut self, realm: Context<'gc>, invoke: &'x dyn Invoke<'gc>) -> Agent<'gc, 'x> {
        Agent { mc: self.mc, roots: self.roots, platform: self.platform, profiler: &mut *self.profiler, realm, invoke }
    }
}

impl<'gc> Agent<'gc, '_> {
    /// 调一个 JS 值。
    pub fn call(&mut self, callee: Value<'gc>, this: Value<'gc>, args: &[Value<'gc>]) -> Result<Value<'gc>, Error> {
        let invoke = self.invoke;
        invoke.call(self, callee, this, args)
    }

    /// 恢复协程一步。
    pub fn resume(&mut self, coroutine: crate::JsObject<'gc>, mode: Resume, value: Value<'gc>) -> Result<Step<'gc>, Error> {
        let invoke = self.invoke;
        invoke.resume(self, coroutine, mode, value)
    }

    /// 构造调用。
    pub fn construct(&mut self, callee: Value<'gc>, args: &[Value<'gc>], new_target: Value<'gc>) -> Result<Value<'gc>, Error> {
        let invoke = self.invoke;
        invoke.construct(self, callee, args, new_target)
    }
}
