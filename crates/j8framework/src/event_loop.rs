//! [`EventLoop`]: one loop drives every isolate's bytecode and the host's async work, on the platform's thread pool
//! through j8taskqueue.
//!
//! Each round, every isolate with something to do becomes one task (a *turn*) in a fresh task queue, and the queue
//! runs the turns in parallel on the platform's pool. A turn:
//!
//! 1. polls the isolate's host futures that were woken (no JS runs here; a ready one becomes a settlement);
//! 2. resumes the running script for the turn's quota of steps; with the quota spent, the turn yields and queues
//!    again behind the other turns (V8's interrupt check, at frame entries and backward jumps);
//! 3. between scripts (the stack empty, the microtasks drained), takes the next macrotask: a submitted script, or a
//!    settlement, which resolves or rejects its promise and drains the microtasks.
//!
//! JS keeps run-to-completion: nothing runs inside a script, only between macrotasks. When no isolate has anything
//! to run, the loop's thread sleeps until a host future wakes it; the loop returns once no future is pending either.
use alloc::boxed::Box;
use alloc::collections::VecDeque;
use alloc::string::String;
use alloc::sync::Arc;
use alloc::task::Wake;
use alloc::vec::Vec;
use core::future::Future;
use core::num::NonZeroUsize;
use core::pin::Pin;
use core::task::{Context, Poll, Waker};
use std::thread::Thread;

use j8lockfree::{AtomicRefCell, Fifo};
use j8taskqueue::{Flow, ITask, ITaskPoll, Quota, TaskQueue};
use moremath::{Execute, Executor};

use crate::platform::Job;
use crate::{Agent, AsyncJob, ContextRoot, Engine, Error, ErrorKind, Global, Invoke, Isolate, Platform, Pool, ScriptKind, Settle, Source, Value};

/// Steps a turn runs before it yields to the other turns
const SLICE: NonZeroUsize = NonZeroUsize::new(10_000).expect("non-zero");
/// Steps a round runs before the loop looks at what it is waiting for again
const ROUND: NonZeroUsize = NonZeroUsize::new(100_000).expect("non-zero");

/// An isolate in an [`EventLoop`]
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct IsolateId(usize);

/// A script submitted to an [`EventLoop`]: its result is taken with [`EventLoop::result`]
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct Ticket(u64);

/// Wakes the loop's thread when a host future can make progress
struct Signal {
    /// The thread that runs the loop (replaced when the loop runs on another one)
    thread: AtomicRefCell<Thread>,
}

impl Signal {
    fn wake(&self) {
        // Busy only while the loop changes threads; it looks at the woken futures before it sleeps
        if let Some(t) = self.thread.try_borrow() {
            t.unpark();
        }
    }

    /// Wakes go to the calling thread from now on
    fn watch(&self) {
        let me = std::thread::current();
        if self.thread.try_borrow().is_some_and(|t| t.id() == me.id()) {
            return;
        }
        loop {
            if let Some(mut t) = self.thread.try_borrow_mut() {
                *t = me;
                return;
            }
            // A waker is reading the old thread: a few instructions
            core::hint::spin_loop();
        }
    }
}

/// The waker of one host future: marks it woken in its isolate and wakes the loop
struct JobWaker {
    job: usize,
    woken: Arc<Fifo<usize>>,
    signal: Arc<Signal>,
}

impl Wake for JobWaker {
    fn wake(self: Arc<Self>) {
        self.wake_by_ref();
    }

    fn wake_by_ref(self: &Arc<Self>) {
        self.woken.push(self.job);
        self.signal.wake();
    }
}

/// A host future not settled yet
struct Pending {
    resolve: Global,
    reject: Global,
    future: Pin<Box<dyn Future<Output = Settle> + Send>>,
    waker: Waker,
}

/// Work an isolate takes between scripts, in order
enum Macrotask {
    Script { ticket: Ticket, text: String, file: String, kind: ScriptKind },
    Settle { resolve: Global, reject: Global, settle: Settle },
}

/// One isolate with everything the loop keeps for it
struct Slot<'p, E: Engine> {
    isolate: Isolate<'p>,
    engine: E,
    cx: ContextRoot,
    macrotasks: VecDeque<Macrotask>,
    running: Option<(Ticket, E::Job)>,
    /// Host futures by index (a settled one leaves a hole for the next)
    pending: Vec<Option<Pending>>,
    holes: Vec<usize>,
    /// Indexes of futures woken since they were last polled
    woken: Arc<Fifo<usize>>,
    finished: Vec<(Ticket, Result<Global, Error>)>,
    /// The first error while settling a promise (reported by the loop)
    fault: Option<Error>,
}

impl<E: Engine + for<'gc> Invoke<'gc>> Slot<'_, E> {
    /// Something to run now
    fn has_work(&self) -> bool {
        self.running.is_some() || !self.macrotasks.is_empty() || !self.woken.is_empty()
    }

    /// Host futures not settled yet
    fn waiting(&self) -> bool {
        self.pending.len() > self.holes.len()
    }

    /// Takes the async host work the isolate queued, to be polled
    fn adopt(&mut self, signal: &Arc<Signal>) {
        for AsyncJob { resolve, reject, future } in self.isolate.take_spawned() {
            let job = self.holes.pop().unwrap_or(self.pending.len());
            let waker = Waker::from(Arc::new(JobWaker { job, woken: Arc::clone(&self.woken), signal: Arc::clone(signal) }));
            let p = Some(Pending { resolve, reject, future, waker });
            match self.pending.get_mut(job) {
                Some(hole) => *hole = p,
                None => self.pending.push(p),
            }
            self.woken.push(job);
        }
    }

    /// Polls the woken futures; each ready one becomes a settlement
    fn poll_woken(&mut self) {
        let mut woken: Vec<usize> = core::iter::from_fn(|| self.woken.pop()).collect();
        woken.sort_unstable();
        woken.dedup();
        for job in woken {
            let Some(Some(p)) = self.pending.get_mut(job) else { continue };
            let ready = p.future.as_mut().poll(&mut Context::from_waker(&p.waker));
            if let Poll::Ready(settle) = ready
                && let Some(Pending { resolve, reject, .. }) = self.pending[job].take()
            {
                self.holes.push(job);
                self.macrotasks.push_back(Macrotask::Settle { resolve, reject, settle });
            }
        }
    }

    /// One turn: see the module documentation
    fn turn(&mut self, signal: &Arc<Signal>, quota: &mut Quota) -> Flow {
        loop {
            self.adopt(signal);
            self.poll_woken();
            if let Some((ticket, job)) = &mut self.running {
                let ticket = *ticket;
                let done = match Engine::resume(&mut self.engine, &mut self.isolate, &self.cx, job, quota) {
                    Ok(None) => return Flow::Yield,
                    Ok(Some(g)) => Ok(g),
                    Err(e) => Err(e),
                };
                self.running = None;
                self.finished.push((ticket, done));
                self.adopt(signal);
                // A script's end ends the turn, so whoever waits for it sees it now
                return Flow::Done;
            }
            if quota.exhausted() {
                return Flow::Yield;
            }
            match self.macrotasks.pop_front() {
                Some(Macrotask::Script { ticket, text, file, kind }) => match Engine::start(&mut self.engine, &mut self.isolate, &self.cx, &Source { text: &text, file: &file }, kind) {
                    Ok(job) => self.running = Some((ticket, job)),
                    Err(e) => self.finished.push((ticket, Err(e))),
                },
                Some(Macrotask::Settle { resolve, reject, settle }) => {
                    quota.spend(1);
                    if let Err(e) = self.settle(&resolve, &reject, settle) {
                        self.fault.get_or_insert(e);
                    }
                }
                None => return Flow::Done,
            }
        }
    }

    /// Resolves (or rejects) a promise with what its host work settled to, then runs the microtasks
    fn settle(&mut self, resolve: &Global, reject: &Global, settle: Settle) -> Result<(), Error> {
        self.enter(|a| {
            let (target, value) = match settle(a) {
                Ok(v) => (resolve, v),
                Err(Error::Exception(g)) => (reject, a.local(&g)),
                Err(e) => {
                    let message = alloc::string::ToString::to_string(&e);
                    let Error::Exception(g) = a.throw(ErrorKind::Error, &message) else { unreachable!("throw makes an exception") };
                    (reject, a.local(&g))
                }
            };
            let f = a.local(target);
            a.call(f, Value::Undefined, &[value])?;
            a.run_jobs()
        })
    }

    fn enter<R>(&mut self, f: impl for<'gc> FnOnce(&mut Agent<'gc, '_>) -> R) -> R {
        let (engine, cx) = (&self.engine, &self.cx);
        self.isolate.enter(|scope| {
            let realm = scope.context_of(cx);
            let mut a = scope.agent(realm, engine);
            f(&mut a)
        })
    }
}

/// An isolate's turn, as a task of the round's queue
struct Turn<'s, 'p, E: Engine> {
    slot: &'s mut Slot<'p, E>,
    signal: &'s Arc<Signal>,
}

impl<'s, E: Engine + Send + for<'gc> Invoke<'gc>> ITask<'s> for Turn<'s, '_, E> {
    fn recursable(&self) -> bool {
        false
    }

    fn exec(&mut self, _: &mut dyn ITaskPoll<'s>, quota: &mut Quota) -> Flow {
        self.slot.turn(self.signal, quota)
    }
}

/// The platform's pool as the executor of a round's queue
struct OnPool<'p>(&'p dyn Pool);

impl Executor for OnPool<'_> {
    fn threads(&self) -> usize {
        self.0.threads()
    }

    fn execute<'x, 't, T: Execute<'t>>(&'x self, tasks: Vec<T>) -> Vec<T::Output> {
        let mut outputs: Vec<Option<T::Output>> = tasks.iter().map(|_| None).collect();
        let jobs: Vec<Job<'_>> = tasks.into_iter().zip(outputs.iter_mut()).map(|(t, out)| Box::new(move || *out = Some(t.execute())) as Job<'_>).collect();
        self.0.run(jobs);
        outputs.into_iter().map(|o| o.expect("the pool runs every job before it returns")).collect()
    }
}

/// ⭐ The event loop: isolates (each with its engine and context), the scripts submitted to them, and the async host
/// work they start, all driven on the platform's pool. Isolates run in parallel; each one runs on one thread at a
/// time, and may move between threads between turns.
pub struct EventLoop<'p, E: Engine> {
    platform: &'p dyn Platform,
    slots: Vec<Slot<'p, E>>,
    signal: Arc<Signal>,
    next: u64,
}

impl<'p, E: Engine + Send + for<'gc> Invoke<'gc>> EventLoop<'p, E> {
    /// A loop with no isolates, running on `platform`'s pool
    pub fn new(platform: &'p dyn Platform) -> Self {
        Self { platform, slots: Vec::new(), signal: Arc::new(Signal { thread: AtomicRefCell::new(std::thread::current()) }), next: 0 }
    }

    /// Takes an isolate, the engine that runs it and the context its scripts run in. The isolate should be made with
    /// [`CreateParams::event_loop`](crate::CreateParams::event_loop), so its async host work comes to this loop.
    pub fn add(&mut self, isolate: Isolate<'p>, engine: E, cx: ContextRoot) -> IsolateId {
        let slot = Slot {
            isolate,
            engine,
            cx,
            macrotasks: VecDeque::new(),
            running: None,
            pending: Vec::new(),
            holes: Vec::new(),
            woken: Arc::default(),
            finished: Vec::new(),
            fault: None,
        };
        self.slots.push(slot);
        IsolateId(self.slots.len() - 1)
    }

    /// Queues `src` to run in isolate `id` after what is queued there already; it runs when the loop runs
    pub fn submit(&mut self, id: IsolateId, src: &Source<'_>, kind: ScriptKind) -> Ticket {
        let ticket = Ticket(self.next);
        self.next += 1;
        self.slots[id.0].macrotasks.push_back(Macrotask::Script { ticket, text: src.text.into(), file: src.file.into(), kind });
        ticket
    }

    /// The result of script `ticket` of isolate `id` once it has finished (taken out: given once)
    pub fn result(&mut self, id: IsolateId, ticket: Ticket) -> Option<Result<Global, Error>> {
        let finished = &mut self.slots[id.0].finished;
        let at = finished.iter().position(|(t, _)| *t == ticket)?;
        Some(finished.swap_remove(at).1)
    }

    /// Submits `src` and runs the loop until it has finished (everything else in the loop runs meanwhile too)
    pub fn eval(&mut self, id: IsolateId, src: &Source<'_>, kind: ScriptKind) -> Result<Global, Error> {
        let ticket = self.submit(id, src, kind);
        self.drive(|lp| lp.slots[id.0].finished.iter().any(|(t, _)| *t == ticket))?;
        self.result(id, ticket).expect("a submitted script finishes before the loop runs dry")
    }

    /// Runs until no isolate has anything to run and no host future is pending, sleeping while only host work is
    /// outstanding. Gives the first error raised while settling a promise.
    pub fn run(&mut self) -> Result<(), Error> {
        self.drive(|_| false)
    }

    /// Works with isolate `id`'s context directly (installing, reading results); async host work started from here
    /// joins the loop
    pub fn enter<R>(&mut self, id: IsolateId, f: impl for<'gc> FnOnce(&mut Agent<'gc, '_>) -> R) -> R {
        let slot = &mut self.slots[id.0];
        let r = slot.enter(f);
        slot.adopt(&self.signal);
        r
    }

    /// Isolate `id` (collecting, the profiler)
    pub fn isolate(&mut self, id: IsolateId) -> &mut Isolate<'p> {
        &mut self.slots[id.0].isolate
    }

    /// Rounds until `stop` holds, the loop runs dry, or settling fails
    fn drive(&mut self, mut stop: impl FnMut(&Self) -> bool) -> Result<(), Error> {
        self.signal.watch();
        loop {
            if let Some(e) = self.slots.iter_mut().find_map(|s| s.fault.take()) {
                return Err(e);
            }
            if stop(self) {
                return Ok(());
            }
            if self.slots.iter().any(Slot::has_work) {
                self.round();
                continue;
            }
            if !self.slots.iter().any(Slot::waiting) {
                return Ok(());
            }
            // Only host work outstanding: sleep until a future wakes (a wake before this point leaves the park token
            // set, or its future already in `woken`)
            std::thread::park();
        }
    }

    /// One round: a turn for every isolate with work, run in parallel until done or the round's budget is spent
    fn round(&mut self) {
        let pool = OnPool(self.platform);
        let mut queue = TaskQueue::from(&pool).with_slice(SLICE).with_budget(ROUND);
        let signal = &self.signal;
        for slot in self.slots.iter_mut().filter(|s| s.has_work()) {
            // The host adds outside `run`: never refused
            let _ = queue.add(Box::new(Turn { slot, signal }));
        }
        // Turns never spawn and yield only with their quota spent: a fault means an engine broke `resume`'s contract
        if let Err(fault) = queue.run() {
            panic!("an engine broke Engine::resume's contract: {fault:?}");
        }
    }
}
