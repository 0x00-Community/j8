use core::fmt;

use r8gc::Root;
use r8gc::{DynamicRoot, Gc, Rootable};

use crate::{Agent, Context, Scope, Value};

/// 持久句柄：跨多次 [`Isolate::enter`](crate::Isolate::enter) 存活，drop 即放掉。
pub struct Global(DynamicRoot<Rootable![Value<'_>]>);

impl fmt::Debug for Global {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("Global")
    }
}

/// 跨多次 [`Isolate::enter`](crate::Isolate::enter) 保留的执行环境。
pub struct ContextRoot(DynamicRoot<Rootable![Context<'_>]>);

impl fmt::Debug for ContextRoot {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("ContextRoot")
    }
}

impl<'gc> Agent<'gc, '_> {
    /// 把任意可回收的东西挂到根上（执行引擎在片与片之间存执行状态用）。
    pub fn stash<R>(&self, v: Gc<'gc, Root<'gc, R>>) -> DynamicRoot<R>
    where
        R: for<'a> Rootable<'a>,
        for<'a> Root<'a, R>: Sized,
    {
        self.roots.stash::<R>(self.mc, v)
    }

    pub fn fetch<R>(&self, root: &DynamicRoot<R>) -> Gc<'gc, Root<'gc, R>>
    where
        R: for<'a> Rootable<'a>,
        for<'a> Root<'a, R>: Sized,
    {
        self.roots.fetch(root)
    }

    pub fn global(&self, v: Value<'gc>) -> Global {
        Global(self.roots.stash(self.mc, Gc::new(self.mc, v)))
    }

    pub fn local(&self, g: &Global) -> Value<'gc> {
        *self.roots.fetch(&g.0)
    }
}

impl<'gc> Scope<'gc, '_> {
    /// 把执行环境存成跨 `enter` 的句柄。
    pub fn persist(&self, cx: Context<'gc>) -> ContextRoot {
        ContextRoot(self.roots.stash::<Rootable![Context<'_>]>(self.mc, Gc::new(self.mc, cx)))
    }

    /// 取回执行环境。
    pub fn context_of(&self, root: &ContextRoot) -> Context<'gc> {
        *self.roots.fetch(&root.0)
    }

    /// 把一个值升成持久句柄。
    pub fn global(&self, v: Value<'gc>) -> Global {
        Global(self.roots.stash(self.mc, Gc::new(self.mc, v)))
    }

    /// 取回持久句柄的值（V8 的 `Local::New(isolate, global)`）。
    pub fn local(&self, g: &Global) -> Value<'gc> {
        *self.roots.fetch(&g.0)
    }
}
