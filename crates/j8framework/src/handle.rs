use core::fmt;

use crate::{Agent, Context, Gc, Realm, Root, Scope, Value};

paragc::brand!(
    pub ValueBrand = for<'scope> Value<'scope>
);
paragc::brand!(
    pub RealmBrand = for<'scope> Realm<'scope>
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

impl<'scope> Agent<'scope, '_> {
    pub fn stash<B: paragc::Brand>(&self, v: Gc<'scope, B::Of<'scope>>) -> Root<B> {
        self.mc.mutator().root(v)
    }

    pub fn fetch<B: paragc::Brand>(&self, root: &Root<B>) -> Gc<'scope, B::Of<'scope>> {
        self.mc.mutator().get(root)
    }

    pub fn global(&self, v: Value<'scope>) -> Global {
        Global(self.mc.mutator().root(self.mc.alloc(v)))
    }

    pub fn local(&self, g: &Global) -> Value<'scope> {
        *self.mc.mutator().get(&g.0)
    }
}

impl<'scope> Scope<'scope, '_> {
    pub fn persist(&self, cx: Context<'scope>) -> ContextRoot {
        ContextRoot(self.mc.mutator().root(cx.realm()))
    }

    pub fn context_of(&self, root: &ContextRoot) -> Context<'scope> {
        Context::from(self.mc.mutator().get(&root.0))
    }

    pub fn global(&self, v: Value<'scope>) -> Global {
        Global(self.mc.mutator().root(self.mc.alloc(v)))
    }

    pub fn local(&self, g: &Global) -> Value<'scope> {
        *self.mc.mutator().get(&g.0)
    }
}
