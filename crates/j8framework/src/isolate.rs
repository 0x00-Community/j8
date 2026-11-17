use core::num::NonZeroUsize;

use crate::{Heap, Mutation, Platform, Profiler};

/// Parameters for creating an isolate.
#[derive(Clone, Copy, Debug, Default)]
pub struct CreateParams {
    /// Whether to enable the profiler
    pub profile: bool,
}

/// An independent JS world: a GC heap (paragc; collected in parallel on its own collector threads, as many as the
/// platform's pool has) plus a profiler; the platform is lent to it by the embedder.
pub struct Isolate<'p> {
    platform: &'p dyn Platform,
    heap: Heap,
    /// Collections completed so far (a [`crate::GcCell`] snapshot from this generation is still young)
    generation: u64,
    profiler: Profiler,
}

/// One entry into an isolate: `'gc` is the brand of this entry, and values inside it can't escape the closure.
pub struct Scope<'gc, 'i> {
    pub mc: &'i Mutation<'gc>,
    pub platform: &'i dyn Platform,
    pub profiler: &'i mut Profiler,
}

impl<'p> Isolate<'p> {
    pub fn new(platform: &'p dyn Platform, params: CreateParams) -> Self {
        let mut profiler = Profiler::default();
        profiler.enable(params.profile);
        let heap = Heap::new(NonZeroUsize::new(platform.threads()).unwrap_or(NonZeroUsize::MIN));
        // Dead objects free memory the mutator allocated (string buffers, vectors): freeing it from the collector
        // threads makes them fight the allocator (glibc's arena lock), so destructors run on the collecting thread only
        heap.set_parallel_drop(false);
        Self { platform, heap, generation: 0, profiler }
    }

    /// How many collection cycles have completed.
    pub fn gc_cycles(&self) -> u64 {
        self.generation
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

    /// Enter the isolate and run `f`; no collection happens during `f`, and afterwards the heap collects if enough has
    /// been allocated (a young collection usually, a full one every few rounds).
    pub fn enter<R>(&mut self, f: impl for<'gc> FnOnce(&mut Scope<'gc, '_>) -> R) -> R {
        let (platform, profiler, generation) = (self.platform, &mut self.profiler, self.generation);
        let r = self.heap.mutate(|m| {
            // SAFETY: built inside this mutate from its own mutator; `generation` is this heap's collection count
            let mc = unsafe { Mutation::new(m, generation) };
            f(&mut Scope { mc: &mc, platform, profiler })
        });
        if self.heap.collect_if_needed() {
            self.generation += 1;
        }
        r
    }

    /// A full collection now (tests, `gc()` hosts)
    pub fn collect(&mut self) {
        self.heap.collect();
        self.generation += 1;
    }
}
