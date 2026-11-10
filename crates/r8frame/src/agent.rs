use gc_arena::{DynamicRootSet, Mutation};

use crate::{Context, Error, Platform, Profiler, Scope, Value};

/// 执行引擎提供的回调：运算（访问器、ToPrimitive、宿主函数）要调回 JS 时经它。
pub trait Invoke<'gc> {
    /// 调一个值；不可调就抛 TypeError。
    fn call(&self, agent: &mut Agent<'gc, '_>, callee: Value<'gc>, this: Value<'gc>, args: &[Value<'gc>]) -> Result<Value<'gc>, Error>;
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
}
