use alloc::boxed::Box;
use alloc::sync::Arc;
use alloc::vec::Vec;
use core::num::NonZeroUsize;
use core::sync::atomic::{AtomicBool, AtomicU8, AtomicUsize, Ordering};

use j8lockfree::Fifo;

use crate::node::{Node, TaskId};
use crate::{Fault, Flow, ITaskPoll, Job, Pool, Quota, Ran, Refused, Stop, Task};

const NO_FAULT: u8 = 0;

struct Shared<'a> {
    ready: Fifo<TaskId<'a>>,
    seq: AtomicUsize,
    spent: AtomicUsize,
    execs: AtomicUsize,
    fault: AtomicU8,
    halt: AtomicBool,
    slice: usize,
    budget: usize,
}

impl<'a> Shared<'a> {
    fn register(&self, task: Task<'a>, spawner: Option<&TaskId<'a>>) -> TaskId<'a> {
        let id = TaskId(Arc::new(Node::new(task, self.seq.fetch_add(1, Ordering::Relaxed))));
        if let Some(p) = spawner {
            Node::union(&id.0, &p.0);
        }
        for d in unsafe { id.0.depend() } {
            Node::union(&id.0, &d.0);
            id.0.hold();
            if d.0.push_succ(id.clone()).is_err() {
                id.0.release();
            }
        }
        if id.0.release() {
            self.ready.push(id.clone());
        }
        id
    }

    fn quota(&self) -> Option<usize> {
        let left = self.budget.saturating_sub(self.spent.load(Ordering::Acquire));
        if left == 0 {
            self.halt.store(true, Ordering::Release);
            return None;
        }
        Some(self.slice.min(left))
    }

    fn fail(&self, fault: Fault) {
        let _ = self.fault.compare_exchange(NO_FAULT, fault.into(), Ordering::AcqRel, Ordering::Acquire);
        self.halt.store(true, Ordering::Release);
    }

    fn step(&self, id: TaskId<'a>, quota: usize) {
        let Some(mut task) = (unsafe { id.0.take() }) else { return };
        if id.0.cancelled() {
            drop(task);
            unsafe { id.0.complete(&self.ready) };
            return;
        }
        let recursable = task.recursable();
        let mut left = Quota::from(quota);
        let flow = task.exec(&mut Spawn { shared: self, me: &id, recursable }, &mut left);
        self.spent.fetch_add(quota - left.left(), Ordering::AcqRel);
        self.execs.fetch_add(1, Ordering::Relaxed);
        let again = match (flow, recursable, left.exhausted()) {
            (Flow::Done, ..) => false,
            (Flow::Again, true, _) | (Flow::Yield, _, true) => true,
            (Flow::Again, false, _) => {
                self.fail(Fault::NotRecursable);
                false
            }
            (Flow::Yield, _, false) => {
                self.fail(Fault::Unspent);
                false
            }
        };
        if again {
            unsafe { id.0.put(task) };
            self.ready.push(id);
        } else {
            drop(task);
            unsafe { id.0.complete(&self.ready) };
        }
    }
}

struct Worker<'s, 'a>(&'s Shared<'a>);

impl Worker<'_, '_> {
    fn work(self) {
        let s = self.0;
        while !s.halt.load(Ordering::Acquire) {
            let Some(quota) = s.quota() else { return };
            let Some(id) = s.ready.pop() else { return };
            s.step(id, quota);
        }
    }
}

struct Spawn<'s, 'n, 'a> {
    shared: &'s Shared<'a>,
    me: &'n TaskId<'a>,
    recursable: bool,
}

impl<'a> ITaskPoll<'a> for Spawn<'_, '_, 'a> {
    fn add(&mut self, task: Task<'a>) -> Result<TaskId<'a>, Refused<'a>> {
        if !self.recursable {
            return Err(Refused(task));
        }
        Ok(self.shared.register(task, Some(self.me)))
    }

    fn run(&mut self) -> Result<Ran, Fault> {
        Err(Fault::Reentrant)
    }
}

pub struct Queue<'a> {
    shared: Shared<'a>,
}

impl Default for Queue<'_> {
    fn default() -> Self {
        let shared = Shared {
            ready: Fifo::default(),
            seq: AtomicUsize::new(0),
            spent: AtomicUsize::new(0),
            execs: AtomicUsize::new(0),
            fault: AtomicU8::new(NO_FAULT),
            halt: AtomicBool::new(false),
            slice: Quota::UNLIMITED,
            budget: Quota::UNLIMITED,
        };
        Self { shared }
    }
}

impl<'a> Queue<'a> {
    pub fn set_slice(&mut self, slice: NonZeroUsize) {
        self.shared.slice = slice.get();
    }

    pub fn set_budget(&mut self, budget: NonZeroUsize) {
        self.shared.budget = budget.get();
    }

    pub fn add(&mut self, task: Task<'a>) -> TaskId<'a> {
        self.shared.register(task, None)
    }

    pub fn is_empty(&self) -> bool {
        self.shared.ready.is_empty()
    }

    pub fn run(&mut self) -> Result<Ran, Fault> {
        self.begin();
        Worker(&self.shared).work();
        self.end()
    }

    pub fn run_on(&mut self, pool: &dyn Pool) -> Result<Ran, Fault> {
        self.begin();
        let shared = &self.shared;
        let jobs: Vec<Job<'_>> = (0..pool.threads().max(1)).map(|_| Box::new(move || Worker(shared).work()) as Job<'_>).collect();
        pool.run(jobs);
        self.end()
    }

    fn begin(&mut self) {
        let s = &mut self.shared;
        *s.spent.get_mut() = 0;
        *s.execs.get_mut() = 0;
        *s.fault.get_mut() = NO_FAULT;
        *s.halt.get_mut() = false;
    }

    fn end(&mut self) -> Result<Ran, Fault> {
        let s = &mut self.shared;
        s.ready.compact();
        if let Ok(fault) = Fault::try_from(*s.fault.get_mut()) {
            return Err(fault);
        }
        let stop = if s.ready.is_empty() { Stop::Empty } else { Stop::Budget };
        Ok(Ran { execs: *s.execs.get_mut(), spent: *s.spent.get_mut(), stop })
    }
}

impl Drop for Queue<'_> {
    fn drop(&mut self) {
        while let Some(id) = self.shared.ready.pop() {
            unsafe { id.0.complete(&self.shared.ready) };
        }
    }
}

pub struct TaskQueue<'x, 'a> {
    pool: &'x dyn Pool,
    queue: Queue<'a>,
}

impl<'x, P: Pool + 'x> From<&'x P> for TaskQueue<'x, '_> {
    fn from(pool: &'x P) -> Self {
        Self { pool, queue: Queue::default() }
    }
}

impl TaskQueue<'_, '_> {
    #[must_use]
    pub fn with_slice(mut self, slice: NonZeroUsize) -> Self {
        self.queue.set_slice(slice);
        self
    }

    #[must_use]
    pub fn with_budget(mut self, budget: NonZeroUsize) -> Self {
        self.queue.set_budget(budget);
        self
    }

    pub fn is_empty(&self) -> bool {
        self.queue.is_empty()
    }
}

impl<'a> ITaskPoll<'a> for TaskQueue<'_, 'a> {
    fn add(&mut self, task: Task<'a>) -> Result<TaskId<'a>, Refused<'a>> {
        Ok(self.queue.add(task))
    }

    fn run(&mut self) -> Result<Ran, Fault> {
        self.queue.run_on(self.pool)
    }
}
