use core::num::NonZeroUsize;

use j8lockfree::Fifo;

use crate::{AsyncJob, Heap, Mutation, Platform, Profiler};

/// Parameters for creating an isolate.
#[derive(Clone, Copy, Debug, Default)]
pub struct CreateParams {
    /// Whether to enable the profiler
    pub profile: bool,
    /// Whether the embedder runs an event loop for async host work ([`Isolate::take_spawned`]); without one,
    /// [`Agent::spawn`](crate::Agent::spawn) refuses the work
    pub event_loop: bool,
    /// Threads the heap collects with (its own collector threads plus the caller); the platform's pool size when
    /// not given. Isolates that run side by side on an event loop's threads want 1 each.
    pub collector_threads: Option<NonZeroUsize>,
}

/// An independent JS world: a GC heap (paragc; collected in parallel on its own collector threads, as many as the
/// platform's pool has) plus a profiler; the platform is lent to it by the embedder.
pub struct Isolate<'p> {
    platform: &'p dyn Platform,
    heap: Heap,
    /// Collections completed so far (a [`crate::GcCell`] snapshot from this generation is still young)
    generation: u64,
    profiler: Profiler,
    /// Async host work waiting for this isolate's event loop (`None`: the embedder runs none)
    spawned: Option<Fifo<AsyncJob>>,
}

/// One entry into an isolate: `'gc` is the brand of this entry, and values inside it can't escape the closure.
pub struct Scope<'gc, 'i> {
    pub mc: &'i Mutation<'gc>,
    pub platform: &'i dyn Platform,
    pub profiler: &'i mut Profiler,
    /// This isolate's queue of async host work (`None` without an event loop)
    pub spawned: Option<&'i Fifo<AsyncJob>>,
}

impl<'p> Isolate<'p> {
    pub fn new(platform: &'p dyn Platform, params: CreateParams) -> Self {
        let mut profiler = Profiler::default();
        profiler.enable(params.profile);
        let heap = Heap::new(params.collector_threads.unwrap_or_else(|| NonZeroUsize::new(platform.threads()).unwrap_or(NonZeroUsize::MIN)));
        // Dead objects free memory the mutator allocated (string buffers, vectors): freeing it from the collector
        // threads makes them fight the allocator (glibc's arena lock), so destructors run on the collecting thread only
        heap.set_parallel_drop(false);
        Self { platform, heap, generation: 0, profiler, spawned: params.event_loop.then(Fifo::default) }
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

    /// The async host work queued since the last call (see [`Agent::spawn`](crate::Agent::spawn)); the event loop
    /// polls it and settles each job's promise in this isolate.
    pub fn take_spawned(&self) -> Vec<AsyncJob> {
        self.spawned.as_ref().map(|q| core::iter::from_fn(|| q.pop()).collect()).unwrap_or_default()
    }

    /// Enter the isolate and run `f`; no collection happens during `f`, and afterwards the heap collects if enough has
    /// been allocated (a young collection usually, a full one every few rounds).
    pub fn enter<R>(&mut self, f: impl for<'gc> FnOnce(&mut Scope<'gc, '_>) -> R) -> R {
        let (platform, profiler, generation, spawned) = (self.platform, &mut self.profiler, self.generation, self.spawned.as_ref());
        let r = self.heap.mutate(|m| {
            // SAFETY: built inside this mutate from its own mutator; `generation` is this heap's collection count
            let mc = unsafe { Mutation::new(m, generation) };
            f(&mut Scope { mc: &mc, platform, profiler, spawned })
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
