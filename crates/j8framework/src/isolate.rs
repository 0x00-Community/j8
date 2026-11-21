use alloc::sync::Arc;
use core::num::NonZeroUsize;

use j8lockfree::Fifo;

use crate::{AsyncJob, Heap, IsolateHandle, Mutation, Platform, Profiler, Termination};

#[derive(Clone, Copy, Debug, Default)]
pub struct CreateParams {
    pub profile: bool,
    pub event_loop: bool,
    pub collector_threads: Option<NonZeroUsize>,
}

pub struct Isolate<'p> {
    platform: &'p dyn Platform,
    heap: Heap,
    generation: u64,
    profiler: Profiler,
    spawned: Option<Fifo<AsyncJob>>,
    stop: Arc<Termination>,
}

pub struct Scope<'gc, 'i> {
    pub mc: &'i Mutation<'gc>,
    pub platform: &'i dyn Platform,
    pub profiler: &'i mut Profiler,
    pub spawned: Option<&'i Fifo<AsyncJob>>,
    pub stop: &'i Termination,
}

impl<'p> Isolate<'p> {
    pub fn new(platform: &'p dyn Platform, params: CreateParams) -> Self {
        let mut profiler = Profiler::default();
        profiler.enable(params.profile);
        let heap = Heap::new(params.collector_threads.unwrap_or_else(|| NonZeroUsize::new(platform.threads()).unwrap_or(NonZeroUsize::MIN)));
        heap.set_parallel_drop(false);
        Self { platform, heap, generation: 0, profiler, spawned: params.event_loop.then(Fifo::default), stop: Arc::default() }
    }

    pub fn handle(&self) -> IsolateHandle {
        IsolateHandle(Arc::clone(&self.stop))
    }

    pub fn gc_cycles(&self) -> u64 {
        self.generation
    }

    pub fn run(&mut self, engine: &mut impl crate::Engine, cx: &crate::ContextRoot, src: &crate::Source<'_>) -> Result<crate::Global, crate::Error> {
        engine.eval(self, cx, src)
    }

    pub fn run_module(&mut self, engine: &mut impl crate::Engine, cx: &crate::ContextRoot, src: &crate::Source<'_>) -> Result<crate::Global, crate::Error> {
        engine.eval_module(self, cx, src)
    }

    pub fn profiler(&self) -> &Profiler {
        &self.profiler
    }

    pub fn take_spawned(&self) -> Vec<AsyncJob> {
        self.spawned.as_ref().map(|q| core::iter::from_fn(|| q.pop()).collect()).unwrap_or_default()
    }

    pub fn enter<R>(&mut self, f: impl for<'gc> FnOnce(&mut Scope<'gc, '_>) -> R) -> R {
        let (platform, profiler, generation, spawned, stop) = (self.platform, &mut self.profiler, self.generation, self.spawned.as_ref(), &*self.stop);
        let r = self.heap.mutate(|m| {
            let mc = unsafe { Mutation::new(m, generation) };
            f(&mut Scope { mc: &mc, platform, profiler, spawned, stop })
        });
        self.stop.finish();
        if self.heap.collect_if_needed() {
            self.generation += 1;
        }
        r
    }

    pub fn collect(&mut self) {
        self.heap.collect();
        self.generation += 1;
    }
}
