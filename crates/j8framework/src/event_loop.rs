//! Event loops. An [`EventLoop`] is one JS event loop: its own isolate, engine and context, and its j8taskqueue
//! queue. Many of them run on a few threads: an [`EventLoopSet`] has loop threads, and every event loop lives on one
//! of them at a time.
//!
//! - **The queue is the event loop.** Its tasks are scripts and settlements (resolve a promise, drain microtasks),
//!   chained so that each waits for the one before (JS runs one task at a time, to completion), and polls of host
//!   futures, which wait for nothing. A script runs a check interval of steps at a time ([`EventLoopSet::set_check`],
//!   V8's interrupt check at frame entries and backward jumps), then yields back to the queue: every time the
//!   bytecode advances, the queue is checked, so host work that became ready meanwhile (also from other threads) is
//!   polled before the script goes on; a ready future queues its settlement at the end of the JS chain.
//! - **Pinned, no claiming.** A thread keeps a local queue of its event loops that have work and gives each a turn
//!   in turn: one run of its task queue on the thread, for the set's slice of steps times the event loop's weight (a
//!   long script does not hold up the other event loops).
//! - **Messages only.** Other threads reach a loop thread by a *wake* (an event loop has something new; deduplicated
//!   by a flag) or a *message* (create an event loop, work in one's context, move one in or out). A message for an
//!   event loop that has moved on is forwarded to where it lives now.
//! - **Load balancing.** New event loops go to the least loaded thread. A thread with nothing to run asks the
//!   busiest one for half the difference, and that thread hands over the event loops waiting in its queue first (work
//!   stealing). [`EventLoopSet::rebalance`] evens the threads out on demand, [`EventLoopSet::split`] starts a new
//!   thread and gives it half of another's. An event loop moves only between turns: its isolate, engine, paused
//!   script and pending futures go with it.
//! - **One quota.** Every turn gets the set's slice of steps times the event loop's weight
//!   ([`EventLoopSet::set_slice`], [`EventLoop::set_weight`]), checking the queue every check interval; the steps
//!   spent are counted per event loop and per thread.
use alloc::boxed::Box;
use alloc::collections::VecDeque;
use alloc::string::String;
use alloc::sync::Arc;
use alloc::task::Wake;
use alloc::vec::Vec;
use core::future::Future;
use core::num::NonZeroUsize;
use core::cell::UnsafeCell;
use core::pin::Pin;
use core::sync::atomic::{AtomicBool, AtomicU64, AtomicUsize, Ordering};
use core::task::{Context, Poll, Waker};
use std::thread::{Scope, Thread};

use j8lockfree::{AtomicRefCell, Fifo, Oneshot, Receiver, SegVec, Sender};
use j8taskqueue::{Flow, ITask, ITaskPoll, Queue, Task, TaskId};

use crate::{Agent, AsyncJob, ContextRoot, CreateParams, Engine, Error, ErrorKind, Global, Intrinsics, Invoke, Isolate, Platform, Quota, ScriptKind, Settle, Source, Value};

/// Steps per turn unless the set says otherwise ([`EventLoopSet::set_slice`])
pub const SLICE: usize = 10_000;
/// Steps a script runs before its event loop's task queue is checked again, unless the set says otherwise
/// ([`EventLoopSet::set_check`])
pub const CHECK: usize = 1_000;
/// A thread with nothing to run pulls from the busiest one when that has at least this many event loops waiting, or
/// this many more event loops
const PULL_GAP: usize = 2;

// ---- What wakers reach ('static) ----

/// One loop thread's doorbell: event loops with something new, the thread to wake, the shutdown flag, its load
struct Door {
    wakes: Fifo<Arc<Bell>>,
    /// The thread that runs the loop
    thread: AtomicRefCell<Thread>,
    closing: AtomicBool,
    /// Event loops living on the thread
    lanes: AtomicUsize,
    /// Event loops waiting in its queue for a turn
    queued: AtomicUsize,
    /// Steps run on the thread
    spent: AtomicU64,
    /// A thread has asked this one for event loops and not been answered yet
    asked: AtomicBool,
}

impl Door {
    fn new() -> Self {
        Self {
            wakes: Fifo::default(),
            thread: AtomicRefCell::new(std::thread::current()),
            closing: AtomicBool::new(false),
            lanes: AtomicUsize::new(0),
            queued: AtomicUsize::new(0),
            spent: AtomicU64::new(0),
            asked: AtomicBool::new(false),
        }
    }

    /// How loaded the thread is: its event loops plus those waiting for a turn
    fn load(&self) -> usize {
        self.lanes.load(Ordering::Relaxed) + self.queued.load(Ordering::Relaxed)
    }

    fn unpark(&self) {
        // Busy only while the loop changes threads; it looks at its inbox before it sleeps
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

/// The doors of a set's threads, by thread index (grows when the set splits)
#[derive(Default)]
struct Doors(SegVec<Arc<Door>>);

impl Doors {
    fn get(&self, i: usize) -> &Door {
        self.0.get(i).expect("a thread of the set")
    }
}

/// An event loop's side that host threads and wakers reach
struct Bell {
    doors: Arc<Doors>,
    /// The thread it lives on (or is moving to)
    home: AtomicUsize,
    /// Its slot among that thread's event loops
    slot: AtomicUsize,
    /// In a thread's wakes or queue
    scheduled: AtomicBool,
    /// Indexes of futures woken since they were last polled
    woken: Fifo<usize>,
    /// Scripts submitted from other threads, in order
    submitted: Fifo<Submitted>,
    /// Scripts not finished plus futures not settled
    outstanding: AtomicUsize,
    /// Errors raised while settling promises
    faults: Fifo<Error>,
    /// Host threads waiting for `outstanding` to reach 0
    idle: Fifo<Thread>,
    /// Its share of the quota: a turn runs the set's slice times this
    weight: AtomicUsize,
    /// Steps it has run
    spent: AtomicU64,
}

impl Bell {
    fn door(&self) -> &Door {
        self.doors.get(self.home.load(Ordering::Acquire))
    }

    /// Something new for the event loop: queue it on its thread (once)
    fn ring(self: &Arc<Self>) {
        if !self.scheduled.swap(true, Ordering::AcqRel) {
            let door = self.door();
            door.wakes.push(Arc::clone(self));
            door.unpark();
        }
    }

    fn begin(&self) {
        self.outstanding.fetch_add(1, Ordering::AcqRel);
    }

    fn end(&self) {
        if self.outstanding.fetch_sub(1, Ordering::AcqRel) == 1 {
            self.wake_idle();
        }
    }

    fn fault(&self, e: Error) {
        self.faults.push(e);
        self.wake_idle();
    }

    fn wake_idle(&self) {
        while let Some(t) = self.idle.pop() {
            t.unpark();
        }
    }

    /// Nothing outstanding, or an error to report
    fn quiet(&self) -> bool {
        self.outstanding.load(Ordering::Acquire) == 0 || !self.faults.is_empty()
    }
}

/// The waker of one host future: marks it woken and rings its event loop
struct JobWaker {
    job: usize,
    bell: Arc<Bell>,
}

impl Wake for JobWaker {
    fn wake(self: Arc<Self>) {
        self.wake_by_ref();
    }

    fn wake_by_ref(self: &Arc<Self>) {
        self.bell.woken.push(self.job);
        self.bell.ring();
    }
}

/// Wakes a host thread blocked on a result
struct ThreadWaker(Thread);

impl Wake for ThreadWaker {
    fn wake(self: Arc<Self>) {
        self.0.unpark();
    }

    fn wake_by_ref(self: &Arc<Self>) {
        self.0.unpark();
    }
}

/// Blocks the calling thread until `rx` has its value
fn block_on<T>(mut rx: Receiver<T>) -> T {
    let waker = Waker::from(Arc::new(ThreadWaker(std::thread::current())));
    let mut cx = Context::from_waker(&waker);
    loop {
        if let Poll::Ready(v) = Pin::new(&mut rx).poll(&mut cx) {
            return v;
        }
        std::thread::park();
    }
}

/// Sends `None` if a message carrying it is dropped without running (its thread shut down)
struct Reply<R>(Option<Sender<Option<R>>>);

impl<R> Reply<R> {
    fn send(mut self, out: R) {
        if let Some(tx) = self.0.take() {
            tx.send(Some(out));
        }
    }
}

impl<R> Drop for Reply<R> {
    fn drop(&mut self) {
        if let Some(tx) = self.0.take() {
            tx.send(None);
        }
    }
}

/// Where a script's result goes
type Answer = Sender<Result<Global, Error>>;

/// A script submitted from any thread
struct Submitted {
    text: String,
    file: String,
    kind: ScriptKind,
    reply: Answer,
}

/// A host future not settled yet
struct Pending {
    resolve: Global,
    reject: Global,
    future: Pin<Box<dyn Future<Output = Settle> + Send>>,
    waker: Waker,
}

// ---- One event loop: a task queue over its JS ----

/// What an event loop's tasks work on: its isolate, engine and context, its host futures, and the tail of its JS
/// chain. Only the event loop's own queue (one task at a time, on the thread the event loop lives on) and the host
/// between two runs of it touch this.
struct JsState<'p, E: Engine> {
    bell: Arc<Bell>,
    isolate: Isolate<'p>,
    engine: E,
    cx: ContextRoot,
    /// Host futures by index (a settled one leaves a hole for the next)
    pending: Vec<Option<Pending>>,
    holes: Vec<usize>,
    /// The last JS task queued (a script or a settlement): the next one waits for it, so JS runs one task at a time
    /// and to completion
    last: Option<TaskId<'p>>,
}

/// [`JsState`] shared by the tasks of one queue
struct Js<'p, E: Engine> {
    state: UnsafeCell<JsState<'p, E>>,
}

// SAFETY: the state is reached only through `Js::state`, whose callers are the tasks of the event loop's own queue
// (run by a single-thread executor, one task at a time) and the event loop's thread between two runs of that queue;
// the state itself moves between threads only with its event loop (`Send`).
unsafe impl<E: Engine> Sync for Js<'_, E> where for<'p> JsState<'p, E>: Send {}

impl<'p, E: Engine + Send + for<'gc> Invoke<'gc> + 'p> Js<'p, E> {
    /// # Safety
    /// The caller is the running task of this event loop's queue, or its thread between two runs of the queue
    #[expect(clippy::mut_from_ref, reason = "exclusive by the queue's protocol, see the type")]
    unsafe fn state(&self) -> &mut JsState<'p, E> {
        // SAFETY: the caller's contract
        unsafe { &mut *self.state.get() }
    }
}

impl<'p, E: Engine + Send + for<'gc> Invoke<'gc> + 'p> JsState<'p, E> {
    /// Queues a JS task after the last one; `add` puts it in the queue
    fn chain(&mut self, add: impl FnOnce(Task<'p>) -> TaskId<'p>, make: impl FnOnce(Vec<TaskId<'p>>) -> Task<'p>) {
        let after = self.last.take().into_iter().collect();
        self.last = Some(add(make(after)));
    }

    /// Takes the async host work the isolate queued while the JS ran; each future gets a first poll
    fn adopt(&mut self, js: &Arc<Js<'p, E>>, mut add: impl FnMut(Task<'p>))
    where
        E: Send + 'p,
    {
        for AsyncJob { resolve, reject, future } in self.isolate.take_spawned() {
            let job = self.holes.pop().unwrap_or(self.pending.len());
            let waker = Waker::from(Arc::new(JobWaker { job, bell: Arc::clone(&self.bell) }));
            let p = Some(Pending { resolve, reject, future, waker });
            match self.pending.get_mut(job) {
                Some(hole) => *hole = p,
                None => self.pending.push(p),
            }
            self.bell.begin();
            add(Box::new(PollTask { js: Arc::clone(js), job }));
        }
    }

    /// A poll task for each future woken since (from any thread)
    fn take_woken(&mut self, js: &Arc<Js<'p, E>>, mut add: impl FnMut(Task<'p>))
    where
        E: Send + 'p,
    {
        let mut woken: Vec<usize> = core::iter::from_fn(|| self.bell.woken.pop()).collect();
        woken.sort_unstable();
        woken.dedup();
        for job in woken {
            add(Box::new(PollTask { js: Arc::clone(js), job }));
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

/// Adds to the running queue (the tasks that add are recursable, so the queue always takes them)
fn add_to<'p>(poll: &mut dyn ITaskPoll<'p>) -> impl FnMut(Task<'p>) -> TaskId<'p> {
    move |t| match poll.add(t) {
        Ok(id) => id,
        Err(_) => unreachable!("event loop tasks that add are recursable"),
    }
}

/// A script: started, then run a check interval at a time. Each time its quota is spent it yields, and the queue
/// first runs what is ready (polls of host futures) before the script goes on.
struct ScriptTask<'p, E: Engine> {
    js: Arc<Js<'p, E>>,
    after: Vec<TaskId<'p>>,
    script: Option<(String, String, ScriptKind)>,
    job: Option<E::Job>,
    reply: Option<Answer>,
}

impl<'p, E: Engine + Send + for<'gc> Invoke<'gc> + 'p> ITask<'p> for ScriptTask<'p, E> {
    fn recursable(&self) -> bool {
        true
    }

    fn depend(&self) -> &[TaskId<'p>] {
        &self.after
    }

    fn exec(&mut self, poll: &mut dyn ITaskPoll<'p>, quota: &mut Quota) -> Flow {
        let js = Arc::clone(&self.js);
        // SAFETY: the running task of the event loop's queue
        let st = unsafe { js.state() };
        if let Some((text, file, kind)) = self.script.take() {
            match Engine::start(&mut st.engine, &mut st.isolate, &st.cx, &Source { text: &text, file: &file }, kind) {
                Ok(job) => self.job = Some(job),
                Err(e) => return self.finish(st, Err(e)),
            }
        }
        let Some(job) = &mut self.job else { return Flow::Done };
        let r = Engine::resume(&mut st.engine, &mut st.isolate, &st.cx, job, quota);
        let mut add = add_to(poll);
        st.adopt(&self.js, |t| drop(add(t)));
        // The check: futures woken from other threads meanwhile get their polls before the script goes on
        st.take_woken(&self.js, |t| drop(add(t)));
        match r {
            Ok(None) => Flow::Yield,
            Ok(Some(g)) => self.finish(st, Ok(g)),
            Err(e) => self.finish(st, Err(e)),
        }
    }
}

impl<E: Engine> ScriptTask<'_, E> {
    fn finish(&mut self, st: &mut JsState<'_, E>, done: Result<Global, Error>) -> Flow {
        if let Some(reply) = self.reply.take() {
            reply.send(done);
        }
        st.bell.end();
        Flow::Done
    }
}

/// A settlement: resolves its promise and drains the microtasks
struct SettleTask<'p, E: Engine> {
    js: Arc<Js<'p, E>>,
    after: Vec<TaskId<'p>>,
    resolve: Global,
    reject: Global,
    settle: Option<Settle>,
}

impl<'p, E: Engine + Send + for<'gc> Invoke<'gc> + 'p> ITask<'p> for SettleTask<'p, E> {
    fn recursable(&self) -> bool {
        true
    }

    fn depend(&self) -> &[TaskId<'p>] {
        &self.after
    }

    fn exec(&mut self, poll: &mut dyn ITaskPoll<'p>, quota: &mut Quota) -> Flow {
        // SAFETY: the running task of the event loop's queue
        let st = unsafe { self.js.state() };
        quota.spend(1);
        if let Some(settle) = self.settle.take()
            && let Err(e) = st.settle(&self.resolve, &self.reject, settle)
        {
            st.bell.fault(e);
        }
        let mut add = add_to(poll);
        st.adopt(&self.js, |t| drop(add(t)));
        st.bell.end();
        Flow::Done
    }
}

/// One poll of a host future (no JS runs); a ready one queues its settlement at the end of the JS chain
struct PollTask<'p, E: Engine> {
    js: Arc<Js<'p, E>>,
    job: usize,
}

impl<'p, E: Engine + Send + for<'gc> Invoke<'gc> + 'p> ITask<'p> for PollTask<'p, E> {
    fn recursable(&self) -> bool {
        true
    }

    fn exec(&mut self, poll: &mut dyn ITaskPoll<'p>, _: &mut Quota) -> Flow {
        // SAFETY: the running task of the event loop's queue
        let st = unsafe { self.js.state() };
        let Some(Some(p)) = st.pending.get_mut(self.job) else { return Flow::Done };
        let ready = p.future.as_mut().poll(&mut Context::from_waker(&p.waker));
        if let Poll::Ready(settle) = ready
            && let Some(Pending { resolve, reject, .. }) = st.pending[self.job].take()
        {
            st.holes.push(self.job);
            let js = Arc::clone(&self.js);
            st.chain(add_to(poll), |after| Box::new(SettleTask { js, after, resolve, reject, settle: Some(settle) }));
        }
        Flow::Done
    }
}

/// One event loop: its task queue and the JS state the tasks share
struct Lane<'p, E: Engine> {
    /// Dropped first: the tasks hold the JS state
    tasks: Queue<'p>,
    js: Arc<Js<'p, E>>,
    bell: Arc<Bell>,
}

impl<'p, E: Engine + Send + for<'gc> Invoke<'gc> + 'p> Lane<'p, E> {
    /// A new event loop for thread `home`: an isolate on `platform` (collecting on the thread it lives on), `engine`,
    /// a context with `intrinsics`
    fn new(platform: &'p dyn Platform, engine: E, intrinsics: &dyn Intrinsics, doors: &Arc<Doors>, home: usize) -> Box<Self> {
        let mut isolate = Isolate::new(platform, CreateParams { event_loop: true, collector_threads: Some(NonZeroUsize::MIN), ..CreateParams::default() });
        let cx = isolate.enter(|scope| scope.persist(scope.context(intrinsics)));
        let bell = Arc::new(Bell {
            doors: Arc::clone(doors),
            home: AtomicUsize::new(home),
            slot: AtomicUsize::new(0),
            scheduled: AtomicBool::new(false),
            woken: Fifo::default(),
            submitted: Fifo::default(),
            outstanding: AtomicUsize::new(0),
            faults: Fifo::default(),
            idle: Fifo::default(),
            weight: AtomicUsize::new(1),
            spent: AtomicU64::new(0),
        });
        let state = JsState { bell: Arc::clone(&bell), isolate, engine, cx, pending: Vec::new(), holes: Vec::new(), last: None };
        Box::new(Self { tasks: Queue::default(), js: Arc::new(Js { state: UnsafeCell::new(state) }), bell })
    }

    /// Something to run now
    fn has_work(&self) -> bool {
        !self.tasks.is_empty() || !self.bell.submitted.is_empty() || !self.bell.woken.is_empty()
    }

    /// Takes in what other threads sent: submitted scripts (on the JS chain) and woken futures (a poll each)
    fn take_in(&mut self) {
        // SAFETY: the event loop's thread, between two runs of its queue
        let st = unsafe { self.js.state() };
        let tasks = &mut self.tasks;
        while let Some(Submitted { text, file, kind, reply }) = self.bell.submitted.pop() {
            let js = Arc::clone(&self.js);
            st.chain(|t| tasks.add(t), |after| Box::new(ScriptTask { js, after, script: Some((text, file, kind)), job: None, reply: Some(reply) }));
        }
        st.take_woken(&self.js, |t| drop(tasks.add(t)));
    }

    /// One turn: runs the queue for `budget` steps, each JS task `check` steps at a time (then the queue is checked
    /// again); gives the steps run
    fn turn(&mut self, budget: NonZeroUsize, check: NonZeroUsize) -> u64 {
        self.take_in();
        self.tasks.set_slice(check);
        self.tasks.set_budget(budget);
        match self.tasks.run() {
            Ok(ran) => ran.spent as u64,
            Err(fault) => panic!("an event loop task broke the task queue's contract: {fault:?}"),
        }
    }

    /// Works in the context directly; async host work started here joins the queue
    fn enter<R>(&mut self, f: impl for<'gc> FnOnce(&mut Agent<'gc, '_>) -> R) -> R {
        // SAFETY: the event loop's thread, between two runs of its queue
        let st = unsafe { self.js.state() };
        let r = st.enter(f);
        let tasks = &mut self.tasks;
        st.adopt(&self.js, |t| drop(tasks.add(t)));
        r
    }
}

// ---- One loop thread ----

/// A closure run with one event loop, wherever it lives (borrowing for `'a`)
type WithLane<'a, 'p, E> = Box<dyn FnOnce(&mut Lane<'p, E>) + Send + 'a>;

/// A closure run on a loop thread (borrowing for `'a`)
type OnThread<'a, 'p, E> = Box<dyn FnOnce(&mut LoopState<'p, E>) + Send + 'a>;

/// What reaches a loop thread besides wakes
enum Msg<'p, E: Engine> {
    /// Make an event loop on this thread
    Spawn(OnThread<'p, 'p, E>),
    /// Run with event loop `bell` (forwarded if it lives elsewhere now)
    With(Arc<Bell>, WithLane<'p, 'p, E>),
    /// An event loop moving in
    Arrive(Box<Lane<'p, E>>),
    /// Hand up to `n` event loops over to thread `to`
    Give { n: usize, to: usize },
    /// Drop event loop `bell`
    Remove(Arc<Bell>),
}

impl<E: Engine> Msg<'_, E> {
    /// The event loop the message is about, if one
    fn bell(&self) -> Option<&Arc<Bell>> {
        match self {
            Self::With(b, _) | Self::Remove(b) => Some(b),
            Self::Arrive(l) => Some(&l.bell),
            Self::Spawn(_) | Self::Give { .. } => None,
        }
    }
}

/// How other threads reach one loop thread
struct Port<'p, E: Engine> {
    inbox: Fifo<Msg<'p, E>>,
    door: Arc<Door>,
}

impl<'p, E: Engine> Port<'p, E> {
    fn send(&self, m: Msg<'p, E>) {
        self.inbox.push(m);
        self.door.unpark();
    }
}

/// What every thread of a set shares
struct Set<'p, E: Engine> {
    platform: &'p dyn Platform,
    doors: Arc<Doors>,
    ports: SegVec<Arc<Port<'p, E>>>,
    /// Held while a thread is added, so doors and ports keep the same indexes
    growing: AtomicBool,
    /// Where the next tie goes
    next: AtomicUsize,
    /// Steps per turn, times each event loop's weight
    slice: AtomicUsize,
    /// Steps a JS task runs before the queue is checked again
    check: AtomicUsize,
}

impl<'p, E: Engine + Send + for<'gc> Invoke<'gc> + 'p> Set<'p, E> {
    fn new(platform: &'p dyn Platform) -> Self {
        Self { platform, doors: Arc::default(), ports: SegVec::default(), growing: AtomicBool::new(false), next: AtomicUsize::new(0), slice: AtomicUsize::new(SLICE), check: AtomicUsize::new(CHECK) }
    }

    fn port(&self, i: usize) -> &Arc<Port<'p, E>> {
        self.ports.get(i).expect("a thread of the set")
    }

    fn threads(&self) -> usize {
        self.ports.len()
    }

    /// A new thread slot: its door and port at the same index
    fn grow(&self) -> usize {
        while self.growing.compare_exchange(false, true, Ordering::Acquire, Ordering::Relaxed).is_err() {
            core::hint::spin_loop();
        }
        let door = Arc::new(Door::new());
        let i = self.doors.0.push(Arc::clone(&door));
        let j = self.ports.push(Arc::new(Port { inbox: Fifo::default(), door }));
        self.growing.store(false, Ordering::Release);
        debug_assert_eq!(i, j, "doors and ports grow together");
        i
    }

    /// The least loaded thread, looking from the next one in turn so ties spread out
    fn least_loaded(&self) -> usize {
        let n = self.threads();
        let start = self.next.fetch_add(1, Ordering::Relaxed);
        (0..n).map(|k| (start + k) % n).min_by_key(|&i| self.doors.get(i).load()).expect("at least one thread")
    }

    /// Runs `f` with event loop `bell` on its thread and gives its result; the calling thread waits
    fn call<R: Send>(&self, bell: &Arc<Bell>, f: impl FnOnce(&mut Lane<'p, E>) -> R + Send) -> R {
        let (tx, rx) = Oneshot::new();
        let reply = Reply(Some(tx));
        let with: WithLane<'_, 'p, E> = Box::new(move |l| reply.send(f(l)));
        // SAFETY: only the lifetime changes. This thread blocks below until the closure has run or been dropped (its
        // `Reply` answers either way), so nothing it borrows ends while it still exists.
        let with = unsafe { core::mem::transmute::<WithLane<'_, 'p, E>, WithLane<'p, 'p, E>>(with) };
        self.port(bell.home.load(Ordering::Acquire)).send(Msg::With(Arc::clone(bell), with));
        block_on(rx).expect("the event loop's thread has shut down")
    }

    /// Asks thread `from` to hand `n` event loops to thread `to`
    fn give(&self, from: usize, to: usize, n: usize) {
        if n > 0 && from != to {
            self.port(from).send(Msg::Give { n, to });
        }
    }
}

/// Everything one loop thread runs: its event loops and their local run queue
struct LoopState<'p, E: Engine> {
    me: usize,
    set: Arc<Set<'p, E>>,
    lanes: Vec<Option<Box<Lane<'p, E>>>>,
    holes: Vec<usize>,
    /// Event loops with work, in turn order
    ready: VecDeque<Arc<Bell>>,
    /// Messages for event loops on their way here
    early: Vec<Msg<'p, E>>,
}

impl<'p, E: Engine + Send + for<'gc> Invoke<'gc> + 'p> LoopState<'p, E> {
    fn new(me: usize, set: Arc<Set<'p, E>>) -> Self {
        Self { me, set, lanes: Vec::new(), holes: Vec::new(), ready: VecDeque::new(), early: Vec::new() }
    }

    fn door(&self) -> &Door {
        self.set.doors.get(self.me)
    }

    /// Event loop `bell`, if it lives here
    fn lane(&mut self, bell: &Arc<Bell>) -> Option<&mut Box<Lane<'p, E>>> {
        if bell.home.load(Ordering::Acquire) != self.me {
            return None;
        }
        self.lanes.get_mut(bell.slot.load(Ordering::Acquire))?.as_mut().filter(|l| Arc::ptr_eq(&l.bell, bell))
    }

    /// Queues `bell` for a turn unless it is queued already
    fn schedule(&mut self, bell: &Arc<Bell>) {
        if !bell.scheduled.swap(true, Ordering::AcqRel) {
            self.ready.push_back(Arc::clone(bell));
        }
    }

    /// Takes an event loop in
    fn insert(&mut self, lane: Box<Lane<'p, E>>) -> Arc<Bell> {
        let bell = Arc::clone(&lane.bell);
        let slot = self.holes.pop().unwrap_or(self.lanes.len());
        bell.slot.store(slot, Ordering::Release);
        bell.home.store(self.me, Ordering::Release);
        let work = lane.has_work();
        match self.lanes.get_mut(slot) {
            Some(hole) => *hole = Some(lane),
            None => self.lanes.push(Some(lane)),
        }
        self.door().lanes.fetch_add(1, Ordering::Relaxed);
        if work {
            // A wake that came while it was moving was dropped (see `pump`): queue it here
            bell.scheduled.store(true, Ordering::Release);
            self.ready.push_back(Arc::clone(&bell));
        }
        // Messages that arrived before it did
        let early = core::mem::take(&mut self.early);
        for m in early {
            self.handle(m);
        }
        bell
    }

    /// Takes event loop `bell` out
    fn take(&mut self, bell: &Arc<Bell>) -> Option<Box<Lane<'p, E>>> {
        self.lane(bell)?;
        let slot = bell.slot.load(Ordering::Acquire);
        let lane = self.lanes[slot].take()?;
        self.holes.push(slot);
        self.door().lanes.fetch_sub(1, Ordering::Relaxed);
        Some(lane)
    }

    /// Hands up to `n` event loops to thread `to`: those waiting for a turn first (work stealing), then any
    fn give(&mut self, n: usize, to: usize) {
        let mut moving = Vec::with_capacity(n);
        while moving.len() < n
            && let Some(bell) = self.ready.pop_back()
        {
            if let Some(l) = self.take(&bell) {
                moving.push(l);
            }
        }
        let mut slot = self.lanes.len();
        while moving.len() < n && slot > 0 {
            slot -= 1;
            let bell = self.lanes[slot].as_ref().map(|l| Arc::clone(&l.bell));
            if let Some(bell) = bell
                && let Some(l) = self.take(&bell)
            {
                moving.push(l);
            }
        }
        let port = Arc::clone(self.set.port(to));
        for lane in moving {
            // From now on wakes and messages for it go to `to` (which also takes the ones that arrive here late)
            lane.bell.home.store(to, Ordering::Release);
            lane.bell.scheduled.store(false, Ordering::Release);
            port.send(Msg::Arrive(lane));
        }
        self.door().asked.store(false, Ordering::Release);
    }

    /// One message: run it, or forward it to where its event loop lives now
    fn handle(&mut self, m: Msg<'p, E>) {
        if let Some(bell) = m.bell()
            && !matches!(m, Msg::Arrive(_))
            && self.lane(bell).is_none()
        {
            let home = bell.home.load(Ordering::Acquire);
            if home == self.me {
                // On its way here: run it once it arrives
                self.early.push(m);
            } else {
                Arc::clone(self.set.port(home)).send(m);
            }
            return;
        }
        match m {
            Msg::Spawn(f) => f(self),
            Msg::With(bell, f) => {
                if let Some(l) = self.lane(&bell) {
                    f(l);
                    if l.has_work() {
                        self.schedule(&bell);
                    }
                }
            }
            Msg::Arrive(lane) => {
                self.insert(lane);
            }
            Msg::Give { n, to } => self.give(n, to),
            Msg::Remove(bell) => {
                self.take(&bell);
            }
        }
    }

    /// Takes in what other threads sent: messages, then wakes
    fn pump(&mut self) {
        let port = Arc::clone(self.set.port(self.me));
        while let Some(m) = port.inbox.pop() {
            self.handle(m);
        }
        while let Some(bell) = port.door.wakes.pop() {
            let home = bell.home.load(Ordering::Acquire);
            if self.lane(&bell).is_some() {
                self.ready.push_back(bell);
            } else if home == self.me {
                // Moving here: `insert` queues it if it has work
                bell.scheduled.store(false, Ordering::Release);
            } else {
                let door = self.set.doors.get(home);
                door.wakes.push(bell);
                door.unpark();
            }
        }
        self.door().queued.store(self.ready.len(), Ordering::Relaxed);
    }

    /// Gives the next event loop with work one turn; `false` when none has work
    fn turn(&mut self) -> bool {
        let Some(bell) = self.ready.pop_front() else { return false };
        let (slice, check) = (self.set.slice.load(Ordering::Relaxed), self.set.check.load(Ordering::Relaxed));
        let Some(l) = self.lane(&bell) else { return true };
        l.bell.scheduled.store(false, Ordering::Release);
        let budget = slice.saturating_mul(l.bell.weight.load(Ordering::Relaxed)).max(1);
        let spent = l.turn(NonZeroUsize::new(budget).unwrap_or(NonZeroUsize::MIN), NonZeroUsize::new(check).unwrap_or(NonZeroUsize::MIN));
        l.bell.spent.fetch_add(spent, Ordering::Relaxed);
        let work = l.has_work();
        self.door().spent.fetch_add(spent, Ordering::Relaxed);
        if work {
            self.schedule(&bell);
        }
        true
    }

    /// With nothing to run: ask the busiest thread for half of the event loops waiting in its queue (work
    /// stealing), or, when none waits there, for half the difference in event loops (once at a time)
    fn pull(&self) {
        let n = self.set.threads();
        if n < 2 {
            return;
        }
        let Some(busiest) = (0..n).filter(|&i| i != self.me).max_by_key(|&i| self.set.doors.get(i).load()) else { return };
        let them = self.set.doors.get(busiest);
        let waiting = them.queued.load(Ordering::Relaxed);
        let (theirs, mine) = (them.lanes.load(Ordering::Relaxed), self.door().lanes.load(Ordering::Relaxed));
        let want = if waiting >= PULL_GAP { waiting / 2 } else if theirs >= mine + PULL_GAP { (theirs - mine) / 2 } else { 0 };
        if want > 0 && !them.asked.swap(true, Ordering::AcqRel) {
            self.set.give(busiest, self.me, want);
        }
    }

    /// The loop: messages, wakes, turns; balances when idle, sleeps when nothing can run, until `done`
    fn serve(&mut self, mut done: impl FnMut(&mut Self) -> bool) {
        self.door().watch();
        loop {
            self.pump();
            if done(self) {
                return;
            }
            if self.turn() {
                continue;
            }
            self.pull();
            let port = self.set.port(self.me);
            // A wake or a message after this check leaves the park token set
            if port.inbox.is_empty() && port.door.wakes.is_empty() {
                std::thread::park();
            }
        }
    }
}

// ---- The public handles ----

/// A submitted script: wait for its result with [`EventLoop::wait`]
pub struct Ticket(Receiver<Result<Global, Error>>);

impl Ticket {
    /// The result if the script has finished (taken: given once)
    pub fn try_take(&mut self) -> Option<Result<Global, Error>> {
        match Pin::new(&mut self.0).poll(&mut Context::from_waker(Waker::noop())) {
            Poll::Ready(r) => Some(r),
            Poll::Pending => None,
        }
    }
}

/// ⭐ One JS event loop: an isolate, its engine and one context, with its scripts and async host work. Made by
/// [`EventLoopSet::spawn`]; it lives on the set's loop threads, and any host thread may use it (each call waits for
/// its answer).
pub struct EventLoop<'p, E: Engine + Send + for<'gc> Invoke<'gc> + 'p> {
    bell: Arc<Bell>,
    set: Arc<Set<'p, E>>,
}

impl<'p, E: Engine + Send + for<'gc> Invoke<'gc> + 'p> EventLoop<'p, E> {
    /// Queues a script after what is queued already
    pub fn submit(&self, src: &Source<'_>, kind: ScriptKind) -> Ticket {
        let (reply, rx) = Oneshot::new();
        self.bell.begin();
        self.bell.submitted.push(Submitted { text: src.text.into(), file: src.file.into(), kind, reply });
        self.bell.ring();
        Ticket(rx)
    }

    /// Waits for a submitted script
    pub fn wait(&self, ticket: Ticket) -> Result<Global, Error> {
        block_on(ticket.0)
    }

    /// Runs `src` to the end (the event loop goes on with its other work meanwhile)
    pub fn eval(&self, src: &Source<'_>, kind: ScriptKind) -> Result<Global, Error> {
        let ticket = self.submit(src, kind);
        self.wait(ticket)
    }

    /// Waits until no script and no host future of this event loop is outstanding; gives the first error raised
    /// while settling a promise
    pub fn run(&self) -> Result<(), Error> {
        let bell = &self.bell;
        while !bell.quiet() {
            bell.idle.push(std::thread::current());
            if bell.quiet() {
                break;
            }
            std::thread::park();
        }
        bell.faults.pop().map_or(Ok(()), Err)
    }

    /// Works in the event loop's context directly (on its thread; the caller waits). Async host work started here
    /// joins the event loop.
    pub fn enter<R: Send>(&self, f: impl for<'gc> FnOnce(&mut Agent<'gc, '_>) -> R + Send) -> R {
        self.set.call(&self.bell, move |l| l.enter(f))
    }

    /// Its share of the set's quota: each turn runs the set's slice times `weight` steps
    pub fn set_weight(&self, weight: NonZeroUsize) {
        self.bell.weight.store(weight.get(), Ordering::Relaxed);
    }

    /// Steps it has run so far
    pub fn spent(&self) -> u64 {
        self.bell.spent.load(Ordering::Relaxed)
    }

    /// The loop thread it lives on now
    pub fn thread(&self) -> usize {
        self.bell.home.load(Ordering::Acquire)
    }
}

impl<'p, E: Engine + Send + for<'gc> Invoke<'gc> + 'p> Drop for EventLoop<'p, E> {
    /// Dropped on its thread (its isolate never leaves the set's threads)
    fn drop(&mut self) {
        self.set.port(self.bell.home.load(Ordering::Acquire)).send(Msg::Remove(Arc::clone(&self.bell)));
    }
}

/// ⭐ Loop threads for many event loops, balanced automatically (see the module documentation)
pub struct EventLoopSet<'s, 'p, E: Engine + Send + for<'gc> Invoke<'gc> + 'p> {
    set: Arc<Set<'p, E>>,
    scope: &'s Scope<'s, 'p>,
}

/// Closes the loop threads when the scope's body ends (even by a panic, so the scope can join them)
struct Close<'p, E: Engine>(Arc<Set<'p, E>>);

impl<E: Engine> Drop for Close<'_, E> {
    fn drop(&mut self) {
        for door in self.0.doors.0.iter() {
            door.closing.store(true, Ordering::Release);
            door.unpark();
        }
    }
}

impl<'p, E: Engine + Send + for<'gc> Invoke<'gc> + 'p> EventLoopSet<'_, 'p, E> {
    /// Runs `body` with `threads` loop threads on `platform`; they stop when `body` returns (event loops still
    /// running then are dropped with their threads)
    pub fn scope<R>(platform: &'p dyn Platform, threads: NonZeroUsize, body: impl for<'a> FnOnce(&EventLoopSet<'a, 'p, E>) -> R) -> R {
        let set = Arc::new(Set::new(platform));
        std::thread::scope::<'p, _, _>(|scope| {
            let lp = EventLoopSet { set: Arc::clone(&set), scope };
            for _ in 0..threads.get() {
                lp.launch();
            }
            let _close = Close(Arc::clone(&set));
            body(&lp)
        })
    }

    /// Starts one more loop thread; gives its index
    fn launch(&self) -> usize {
        let me = self.set.grow();
        let set = Arc::clone(&self.set);
        self.scope.spawn(move || {
            let mut state = LoopState::new(me, Arc::clone(&set));
            state.serve(|s| s.door().closing.load(Ordering::Acquire));
        });
        me
    }

    /// Loop threads
    pub fn threads(&self) -> usize {
        self.set.threads()
    }

    /// Each thread's load (its event loops plus those waiting for a turn), by thread
    pub fn loads(&self) -> Vec<usize> {
        (0..self.threads()).map(|i| self.set.doors.get(i).load()).collect()
    }

    /// Steps each thread has run, by thread
    pub fn spent(&self) -> Vec<u64> {
        (0..self.threads()).map(|i| self.set.doors.get(i).spent.load(Ordering::Relaxed)).collect()
    }

    /// Steps per turn for every event loop of the set (times its weight)
    pub fn set_slice(&self, steps: NonZeroUsize) {
        self.set.slice.store(steps.get(), Ordering::Relaxed);
    }

    /// Steps a script runs before its event loop's task queue is checked again (host work that is ready runs then)
    pub fn set_check(&self, steps: NonZeroUsize) {
        self.set.check.store(steps.get(), Ordering::Relaxed);
    }

    /// A new event loop with `engine` and a context with `intrinsics`, on the least loaded thread. Its isolate is
    /// created there and collects on the thread it lives on.
    pub fn spawn(&self, engine: E, intrinsics: &(dyn Intrinsics + Sync)) -> EventLoop<'p, E> {
        let home = self.set.least_loaded();
        let (tx, rx) = Oneshot::new();
        let reply = Reply(Some(tx));
        let platform = self.set.platform;
        let make: OnThread<'_, 'p, E> = Box::new(move |s| {
            let lane = Lane::new(platform, engine, intrinsics, &s.set.doors, s.me);
            reply.send(s.insert(lane));
        });
        // SAFETY: only the lifetime changes. This thread blocks below until the closure has run or been dropped (its
        // `Reply` answers either way), so `intrinsics` outlives it.
        let make = unsafe { core::mem::transmute::<OnThread<'_, 'p, E>, OnThread<'p, 'p, E>>(make) };
        self.set.port(home).send(Msg::Spawn(make));
        let bell = block_on(rx).expect("the loop thread has shut down");
        EventLoop { bell, set: Arc::clone(&self.set) }
    }

    /// Evens the threads out now: each thread above the average gives its excess to the threads below it. Gives how
    /// many event loops were asked to move.
    pub fn rebalance(&self) -> usize {
        let mut loads = self.loads();
        let n = loads.len();
        if n < 2 {
            return 0;
        }
        let avg = loads.iter().sum::<usize>().div_ceil(n);
        let mut moved = 0;
        for from in 0..n {
            for to in 0..n {
                if loads[from] <= avg {
                    break;
                }
                let k = (loads[from] - avg).min(avg.saturating_sub(loads[to]));
                if k > 0 {
                    self.set.give(from, to, k);
                    loads[from] -= k;
                    loads[to] += k;
                    moved += k;
                }
            }
        }
        moved
    }

    /// Starts a new loop thread and gives it half of thread `thread`'s event loops; gives the new thread's index
    pub fn split(&self, thread: usize) -> usize {
        let new = self.launch();
        let half = self.set.doors.get(thread).lanes.load(Ordering::Relaxed) / 2;
        self.set.give(thread, new, half);
        new
    }
}
