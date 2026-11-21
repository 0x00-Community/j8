use core::fmt;

use crate::{Agent, Context, Gc, Realm, Root, Scope, Value};

paragc::brand!(
    pub ValueBrand = for<'gc> Value<'gc>
);
paragc::brand!(
    pub RealmBrand = for<'gc> Realm<'gc>
);

pub struct Global(Root<ValueBrand>);

impl fmt::Debug for Global {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("Global")
    }
}

pub struct ContextRoot(Root<RealmBrand>);

impl fmt::Debug for ContextRoot {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("ContextRoot")
    }
}

impl<'gc> Agent<'gc, '_> {
    pub fn stash<B: paragc::Brand>(&self, v: Gc<'gc, B::Of<'gc>>) -> Root<B> {
        self.mc.mutator().root(v)
    }

    pub fn fetch<B: paragc::Brand>(&self, root: &Root<B>) -> Gc<'gc, B::Of<'gc>> {
        self.mc.mutator().get(root)
    }

    pub fn global(&self, v: Value<'gc>) -> Global {
        Global(self.mc.mutator().root(self.mc.alloc(v)))
    }

    pub fn local(&self, g: &Global) -> Value<'gc> {
        *self.mc.mutator().get(&g.0)
    }
}

impl<'gc> Scope<'gc, '_> {
    pub fn persist(&self, cx: Context<'gc>) -> ContextRoot {
        ContextRoot(self.mc.mutator().root(cx.realm()))
    }

    pub fn context_of(&self, root: &ContextRoot) -> Context<'gc> {
        Context::from(self.mc.mutator().get(&root.0))
    }

    pub fn global(&self, v: Value<'gc>) -> Global {
        Global(self.mc.mutator().root(self.mc.alloc(v)))
    }

    pub fn local(&self, g: &Global) -> Value<'gc> {
        *self.mc.mutator().get(&g.0)
    }
}
