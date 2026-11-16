use j8gc::{Collect, DynamicRootSet, Mutation, J8Heap, Rootable};

use crate::{Platform, Profiler};

/// Parameters for creating an isolate.
#[derive(Clone, Copy, Debug, Default)]
pub struct CreateParams {
    /// Whether to enable the profiler
    pub profile: bool,
}

/// The root of the GC heap: all persistent handles hang off it.
#[derive(Collect)]
#[collect(no_drop)]
struct Roots<'gc> {
    set: DynamicRootSet<'gc>,
}

/// An independent JS world: a GC heap plus a profiler; the platform is lent to it by the embedder (the GC runs in
/// parallel on the platform's thread pool).
pub struct Isolate<'p> {
    platform: &'p dyn Platform,
    arena: J8Heap<'p, Rootable![Roots<'_>]>,
    profiler: Profiler,
}

/// One entry into an isolate: `'gc` is the brand of this entry, and values inside it can't escape the closure.
pub struct Scope<'gc, 'i> {
    pub mc: &'gc Mutation<'gc>,
    pub(crate) roots: &'gc DynamicRootSet<'gc>,
    pub platform: &'i dyn Platform,
    pub profiler: &'i mut Profiler,
}

impl<'p> Isolate<'p> {
    pub fn new(platform: &'p dyn Platform, params: CreateParams) -> Self {
        let mut profiler = Profiler::default();
        profiler.enable(params.profile);
        let arena = J8Heap::new(platform, |mc| Roots { set: DynamicRootSet::new(mc) });
        Self { platform, arena, profiler }
    }

    /// How many collection cycles have completed.
    pub fn gc_cycles(&self) -> usize {
        self.arena.metrics().total_gc_count()
    }

    /// Run a script with `engine` in execution context `cx`.
    pub fn run(&mut self, engine: &mut impl crate::Engine, cx: &crate::ContextRoot, src: &crate::Source<'_>) -> Result<crate::Global, crate::Error> {
        engine.eval(self, cx, src)
    }

    /// Run a module (see [`Engine::eval_module`](crate::Engine::eval_module)).
    pub fn run_module(&mut self, engine: &mut impl crate::Engine, cx: &crate::ContextRoot, src: &crate::Source<'_>) -> Result<crate::Global, crate::Error> {
        engine.eval_module(self, cx, src)
    }

    pub fn profiler(&self) -> &Profiler {
        &self.profiler
    }

    /// Enter the isolate and run `f`; no collection happens during `f`, and afterwards an incremental collection slice
    /// is done according to the accrued debt.
    pub fn enter<R>(&mut self, f: impl for<'gc> FnOnce(&mut Scope<'gc, '_>) -> R) -> R {
        let (platform, profiler) = (self.platform, &mut self.profiler);
        let r = self.arena.mutate(|mc, roots| f(&mut Scope { mc, roots: &roots.set, platform, profiler }));
        self.arena.collect_debt();
        r
    }
}
