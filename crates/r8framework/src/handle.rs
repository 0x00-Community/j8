use core::fmt;

use r8gc::Root;
use r8gc::{DynamicRoot, Gc, Rootable};

use crate::{Agent, Context, Scope, Value};

/// A persistent handle: survives across multiple [`Isolate::enter`](crate::Isolate::enter) calls and is released on
/// drop.
pub struct Global(DynamicRoot<Rootable![Value<'_>]>);

impl fmt::Debug for Global {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("Global")
    }
}

/// An execution context kept across multiple [`Isolate::enter`](crate::Isolate::enter) calls.
pub struct ContextRoot(DynamicRoot<Rootable![Context<'_>]>);

impl fmt::Debug for ContextRoot {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("ContextRoot")
    }
}

impl<'gc> Agent<'gc, '_> {
    /// Attach anything collectable to the root (the execution engine uses it to store execution state between slices).
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
    /// Store an execution context as a handle that survives across `enter`.
    pub fn persist(&self, cx: Context<'gc>) -> ContextRoot {
        ContextRoot(self.roots.stash::<Rootable![Context<'_>]>(self.mc, Gc::new(self.mc, cx)))
    }

    /// Get the execution context back.
    pub fn context_of(&self, root: &ContextRoot) -> Context<'gc> {
        *self.roots.fetch(&root.0)
    }

    /// Promote a value to a persistent handle.
    pub fn global(&self, v: Value<'gc>) -> Global {
        Global(self.roots.stash(self.mc, Gc::new(self.mc, v)))
    }

    /// Get the value of a persistent handle back (V8's `Local::New(isolate, global)`).
    pub fn local(&self, g: &Global) -> Value<'gc> {
        *self.roots.fetch(&g.0)
    }
}
