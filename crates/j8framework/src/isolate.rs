use alloc::sync::Arc;
use core::num::NonZeroUsize;

use j8lockfree::Fifo;

use crate::{AsyncJob, Heap, IsolateHandle, Mutation, Platform, Profiler, Termination};
#[cfg(all(target_os = "linux", feature = "crossheap"))]
use crate::{Erased, Gc, Root, erase};

#[cfg(all(target_os = "linux", feature = "crossheap"))]
paragc::brand!(pub ErasedBrand = for<'gc> Erased);

#[cfg(all(target_os = "linux", feature = "crossheap"))]
#[derive(Default)]
pub struct Shares {
    cross: Option<paragc::CrossHeap>,
    next: u64,
    pending: Vec<(u64, Root<ErasedBrand>)>,
    signed: Vec<(u64, Option<paragc::Signature>)>,
}

#[cfg(all(target_os = "linux", feature = "crossheap"))]
impl Shares {
    pub fn share<'gc, T>(&mut self, mc: &Mutation<'gc>, gc: Gc<'gc, T>) -> u64 {
        let ticket = self.next;
        self.next += 1;
        self.pending.push((ticket, mc.mutator().root::<ErasedBrand>(erase(gc))));
        ticket
    }

    fn due(&self, heap: &Heap) -> bool {
        self.cross.is_some() && (heap.promotion_pending() || !self.pending.is_empty())
    }

    fn settle(&mut self, heap: &Heap) {
        let Some(cross) = &self.cross else { return };
        let pending = core::mem::take(&mut self.pending);
        let signed: Vec<_> = heap.mutate(|m| cross.mutate(|cm| pending.iter().map(|(ticket, root)| (*ticket, cm.sign_promoted(m.get(root)))).collect()));
        self.signed.extend(signed);
    }
}

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
    #[cfg(all(target_os = "linux", feature = "crossheap"))]
    shares: Shares,
}

pub struct Scope<'gc, 'i> {
    pub mc: &'i Mutation<'gc>,
    pub platform: &'i dyn Platform,
    pub profiler: &'i mut Profiler,
    pub spawned: Option<&'i Fifo<AsyncJob>>,
    pub stop: &'i Termination,
    #[cfg(all(target_os = "linux", feature = "crossheap"))]
    pub shares: &'i mut Shares,
}

impl<'p> Isolate<'p> {
    pub fn new(platform: &'p dyn Platform, params: CreateParams) -> Self {
        let mut profiler = Profiler::default();
        profiler.enable(params.profile);
        let heap = Heap::new(params.collector_threads.unwrap_or_else(|| NonZeroUsize::new(platform.threads()).unwrap_or(NonZeroUsize::MIN)));
        heap.set_parallel_drop(false);
        Self {
            platform,
            heap,
            generation: 0,
            profiler,
            spawned: params.event_loop.then(Fifo::default),
            stop: Arc::default(),
            #[cfg(all(target_os = "linux", feature = "crossheap"))]
            shares: Shares::default(),
        }
    }

    #[cfg(all(target_os = "linux", feature = "crossheap"))]
    pub fn attach_cross_heap(&mut self, cross: paragc::CrossHeap) -> std::io::Result<()> {
        self.heap.attach_cross_heap(&cross)?;
        self.shares.cross = Some(cross);
        Ok(())
    }

    #[cfg(all(target_os = "linux", feature = "crossheap"))]
    pub fn cross_heap(&self) -> Option<&paragc::CrossHeap> {
        self.shares.cross.as_ref()
    }

    #[cfg(all(target_os = "linux", feature = "crossheap"))]
    pub fn take_signatures(&mut self) -> Vec<(u64, Option<paragc::Signature>)> {
        core::mem::take(&mut self.shares.signed)
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
        #[cfg(all(target_os = "linux", feature = "crossheap"))]
        let shares = &mut self.shares;
        let r = self.heap.mutate(|m| {
            let mc = unsafe { Mutation::new(m, generation) };
            f(&mut Scope {
                mc: &mc,
                platform,
                profiler,
                spawned,
                stop,
                #[cfg(all(target_os = "linux", feature = "crossheap"))]
                shares,
            })
        });
        self.stop.finish();
        #[cfg(all(target_os = "linux", feature = "crossheap"))]
        if self.shares.due(&self.heap) {
            self.heap.collect_young();
            self.generation += 1;
            self.shares.settle(&self.heap);
        }
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
