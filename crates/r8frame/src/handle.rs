use core::fmt;

use gc_arena::{DynamicRoot, Gc, Rootable};

use crate::{Agent, Scope, Value};

/// 持久句柄：跨多次 [`Isolate::enter`](crate::Isolate::enter) 存活，drop 即放掉。
pub struct Global(DynamicRoot<Rootable![Value<'_>]>);

impl fmt::Debug for Global {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("Global")
    }
}

impl<'gc> Agent<'gc, '_> {
    pub fn global(&self, v: Value<'gc>) -> Global {
        Global(self.roots.stash(self.mc, Gc::new(self.mc, v)))
    }

    pub fn local(&self, g: &Global) -> Value<'gc> {
        *self.roots.fetch(&g.0)
    }
}

impl<'gc> Scope<'gc, '_> {
    /// 把一个值升成持久句柄。
    pub fn global(&self, v: Value<'gc>) -> Global {
        Global(self.roots.stash(self.mc, Gc::new(self.mc, v)))
    }

    /// 取回持久句柄的值（V8 的 `Local::New(isolate, global)`）。
    pub fn local(&self, g: &Global) -> Value<'gc> {
        *self.roots.fetch(&g.0)
    }
}
