use core::fmt;

use crate::{Agent, Context, Realm, Scope, ScopeGlobal, ScopeLocal, Value};

paragc::brand!(
    pub ValueBrand = for<'heap, 'scope> Value<'scope>
);
paragc::brand!(
    pub RealmBrand = for<'heap, 'scope> Realm<'scope>
);

pub struct Global<'i>(ScopeGlobal<'i, ValueBrand>);

impl fmt::Debug for Global<'_> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("Global")
    }
}

pub struct ContextRoot<'i>(ScopeGlobal<'i, RealmBrand>);

impl fmt::Debug for ContextRoot<'_> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("ContextRoot")
    }
}

impl<'scope> Agent<'scope, '_> {
    pub fn stash<'i, B: paragc::Brand>(&self, v: ScopeLocal<'scope, B::Of<'scope, 'scope>>) -> ScopeGlobal<'i, B> {
        self.mc.stash(v)
    }

    pub fn fetch<B: paragc::Brand>(&self, root: &ScopeGlobal<'_, B>) -> ScopeLocal<'scope, B::Of<'scope, 'scope>> {
        self.mc.fetch(root)
    }

    pub fn global<'i>(&self, v: Value<'scope>) -> Global<'i> {
        Global(self.mc.stash(self.mc.alloc(v)))
    }

    pub fn local(&self, g: &Global<'_>) -> Value<'scope> {
        *self.mc.fetch(&g.0)
    }
}

impl<'scope> Scope<'scope, '_> {
    pub fn persist<'i>(&self, cx: Context<'scope>) -> ContextRoot<'i> {
        ContextRoot(self.mc.stash(cx.realm()))
    }

    pub fn context_of(&self, root: &ContextRoot<'_>) -> Context<'scope> {
        Context::from(self.mc.fetch(&root.0))
    }

    pub fn global<'i>(&self, v: Value<'scope>) -> Global<'i> {
        Global(self.mc.stash(self.mc.alloc(v)))
    }

    pub fn local(&self, g: &Global<'_>) -> Value<'scope> {
        *self.mc.fetch(&g.0)
    }
}
