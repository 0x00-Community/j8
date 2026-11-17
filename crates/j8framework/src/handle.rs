use core::fmt;

use crate::{Agent, Context, Gc, Realm, Root, Scope, Value};

paragc::brand!(
    /// The brand of a value boxed on the heap for a [`Global`]
    pub ValueBrand = for<'gc> Value<'gc>
);
paragc::brand!(
    /// The brand of a realm, for a [`ContextRoot`]
    pub RealmBrand = for<'gc> Realm<'gc>
);

/// A persistent handle: survives across multiple [`Isolate::enter`](crate::Isolate::enter) calls and is released on
/// drop.
pub struct Global(Root<ValueBrand>);

impl fmt::Debug for Global {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("Global")
    }
}

/// An execution context kept across multiple [`Isolate::enter`](crate::Isolate::enter) calls.
pub struct ContextRoot(Root<RealmBrand>);

impl fmt::Debug for ContextRoot {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("ContextRoot")
    }
}

impl<'gc> Agent<'gc, '_> {
    /// Attach anything collectable to the roots (the execution engine uses it to store execution state between slices).
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
    /// Store an execution context as a handle that survives across `enter`.
    pub fn persist(&self, cx: Context<'gc>) -> ContextRoot {
        ContextRoot(self.mc.mutator().root(cx.realm()))
    }

    /// Get the execution context back.
    pub fn context_of(&self, root: &ContextRoot) -> Context<'gc> {
        Context::from(self.mc.mutator().get(&root.0))
    }

    /// Promote a value to a persistent handle.
    pub fn global(&self, v: Value<'gc>) -> Global {
        Global(self.mc.mutator().root(self.mc.alloc(v)))
    }

    /// Get the value of a persistent handle back (V8's `Local::New(isolate, global)`).
    pub fn local(&self, g: &Global) -> Value<'gc> {
        *self.mc.mutator().get(&g.0)
    }
}
