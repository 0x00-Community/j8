use crate::{Agent, Context, Scope, ScopeGlobal, Value};

paragc::brand!(
    pub ValueBrand = for<'heap, 'scope> Value<'scope>
);
paragc::brand!(
    pub ContextBrand = for<'heap, 'scope> Context<'scope>
);

impl<'scope> Agent<'scope, '_> {
    pub fn global<'i>(&self, v: Value<'scope>) -> ScopeGlobal<'i, ValueBrand> {
        self.mc.stash(self.mc.alloc(v))
    }

    pub fn local(&self, g: &ScopeGlobal<'_, ValueBrand>) -> Value<'scope> {
        *self.mc.fetch(g)
    }
}

impl<'scope> Scope<'scope, '_> {
    pub fn global<'i>(&self, v: Value<'scope>) -> ScopeGlobal<'i, ValueBrand> {
        self.mc.stash(self.mc.alloc(v))
    }

    pub fn local(&self, g: &ScopeGlobal<'_, ValueBrand>) -> Value<'scope> {
        *self.mc.fetch(g)
    }
}
