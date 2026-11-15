//! r8taskqueue：任务队列与事件循环（`no_std` + `alloc`），每一轮的任务在 moremath 的 [`Executor`] 上并行跑。
//!
//! - [`ITask`]：一个任务。[`ITask::recursable`] 说它能不能在执行时塞新任务（或要求下一轮再跑自己），[`ITask::depend`] 说它要等哪些任务先做完，[`ITask::exec`] 执行它
//! - [`ITaskPoll`]：队列。[`ITaskPoll::add`] 塞一个任务、拿到它的 [`TaskId`]，[`ITaskPoll::run`] 启动事件循环，直到没有任务
//! - [`TaskQueue`]：[`ITaskPoll`] 的实现，借用一个 [`Executor`]（宿主给 `ThreadMulti` 线程池；`no_std` 下可用 `ThreadSingle`）
//!
//! 设计：
//! - **按轮推进、整轮并行**：一轮开始时把待跑的任务整批交给 [`Executor::execute`]，同一轮的任务互不等待、并行执行；
//!   这一轮里新塞的任务（和要求再跑的任务）进下一轮。每一轮必定跑完，任务不断塞新任务只会让轮数一直涨
//! - **依赖**：一轮只跑依赖都已做完的任务（依赖的编号不在队列里了就算做完），其余留到下一轮、次序不变；
//!   JS 的 promise job 要共用一个堆、按 FIFO 跑，就让每个 job 依赖前一个，串成一条链，和不相干的任务照样并行。
//!   剩下的任务全在互相等（循环依赖、依赖自己）时 [`ITaskPoll::run`] 报 [`Fault::Deadlock`]
//! - **次序确定**：任务执行时拿到的是自己的收集器（也实现 [`ITaskPoll`]），新塞的任务先记在里面；
//!   整轮跑完后按任务原来的次序并进下一轮，和线程怎么调度无关
//! - **借用而非 `'static`**：任务活在 `'a` 里，可以借用调用方的数据，只要求 `Send`
//! - **没有时间概念**：不排延时、不睡眠等待；队列空了 [`ITaskPoll::run`] 就返回，什么时候再跑由宿主决定
//! - **谁能塞任务**：宿主在循环外随时能塞；循环里只有可递归的任务能塞，不可递归的任务塞会被退回（[`Refused`] 带回原任务）
//! - **不重入**：任务拿到的收集器上调 [`ITaskPoll::run`] 得到 [`Fault::Reentrant`]
//!
//! ```
//! use std::sync::Mutex;
//! use moremath::ThreadMulti;
//! use r8taskqueue::{Flow, ITask, ITaskPoll, TaskQueue};
//!
//! /// 往 `log` 里记一笔；`fanout` 不为零时再塞两个下一级
//! struct Note<'a> { log: &'a Mutex<Vec<u32>>, id: u32, fanout: u32 }
//!
//! impl<'a> ITask<'a> for Note<'a> {
//!     fn recursable(&self) -> bool { self.fanout != 0 }
//!     fn exec(&mut self, poll: &mut dyn ITaskPoll<'a>) -> Flow {
//!         self.log.lock().unwrap().push(self.id);
//!         for k in 1..=2 * u32::from(self.fanout != 0) {
//!             let child = Note { log: self.log, id: self.id * 10 + k, fanout: self.fanout - 1 };
//!             assert!(poll.add(Box::new(child)).is_ok());
//!         }
//!         Flow::Done
//!     }
//! }
//!
//! let log = Mutex::new(Vec::new());
//! let pool = ThreadMulti::default();
//! let mut queue = TaskQueue::from(&pool);
//! assert!(queue.add(Box::new(Note { log: &log, id: 1, fanout: 2 })).is_ok());
//! assert!(queue.add(Box::new(Note { log: &log, id: 2, fanout: 0 })).is_ok());
//! let ran = queue.run().unwrap();
//! drop(queue);
//! let mut log = log.into_inner().unwrap();
//! log.sort_unstable();
//! assert_eq!(log, [1, 2, 11, 12, 111, 112, 121, 122]);
//! assert_eq!((ran.rounds, ran.execs), (3, 8));
//! ```
//!
//! 依赖串成链：一开始一起塞进去，也按链上的次序一轮一个地跑。
//!
//! ```
//! use std::sync::Mutex;
//! use moremath::ThreadMulti;
//! use r8taskqueue::{Flow, ITask, ITaskPoll, TaskId, TaskQueue};
//!
//! struct Job<'a> { log: &'a Mutex<Vec<char>>, name: char, after: Vec<TaskId> }
//!
//! impl<'a> ITask<'a> for Job<'a> {
//!     fn recursable(&self) -> bool { false }
//!     fn depend(&self) -> &[TaskId] { &self.after }
//!     fn exec(&mut self, _: &mut dyn ITaskPoll<'a>) -> Flow {
//!         self.log.lock().unwrap().push(self.name);
//!         Flow::Done
//!     }
//! }
//!
//! let log = Mutex::new(Vec::new());
//! let pool = ThreadMulti::default();
//! let mut queue = TaskQueue::from(&pool);
//! let a = queue.add(Box::new(Job { log: &log, name: 'a', after: vec![] })).unwrap();
//! let c = Job { log: &log, name: 'c', after: vec![] };
//! let b = queue.add(Box::new(Job { log: &log, name: 'b', after: vec![a] })).unwrap();
//! let c = queue.add(Box::new(Job { after: vec![b], ..c })).unwrap();
//! queue.add(Box::new(Job { log: &log, name: 'd', after: vec![c] })).unwrap();
//! assert_eq!(queue.run().unwrap().rounds, 4);
//! drop(queue);
//! assert_eq!(log.into_inner().unwrap(), ['a', 'b', 'c', 'd']);
//! ```
#![no_std]
extern crate alloc;

use alloc::boxed::Box;
use alloc::collections::BTreeSet;
use alloc::vec::Vec;
use core::fmt;
use core::mem;
use core::sync::atomic::{AtomicUsize, Ordering};

use moremath::{Execute, Executor};

/// 队列里存的任务：借用调用方数据，活不过 `'a`。
pub type Task<'a> = Box<dyn ITask<'a> + 'a>;

/// 一个任务；同一轮的任务可能在不同线程上执行，所以要 `Send`。
pub trait ITask<'a>: Send {
    /// 能否在执行时往队列里塞新任务、能否返回 [`Flow::Again`]。
    fn recursable(&self) -> bool;

    /// 要等哪些任务先做完；缺省不等。
    fn depend(&self) -> &[TaskId] {
        &[]
    }

    /// 执行一次。可递归的任务经 `poll` 塞新任务（进下一轮）。
    fn exec(&mut self, poll: &mut dyn ITaskPoll<'a>) -> Flow;
}

/// 塞进队列时发的编号，一个 [`TaskQueue`] 里不重复；[`Flow::Again`] 再跑时编号不变。
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct TaskId(usize);

/// 任务执行完之后怎么办。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Flow {
    /// 做完了，丢掉
    Done,
    /// 下一轮再跑一次（只有可递归的任务能这样）
    Again,
}

/// 任务队列。
pub trait ITaskPoll<'a> {
    /// 塞一个任务，进下一轮，返回它的编号（给别的任务的 [`ITask::depend`] 用）。循环里由不可递归的任务塞时退回。
    fn add(&mut self, task: Task<'a>) -> Result<TaskId, Refused<'a>>;

    /// 启动事件循环：一轮一轮跑，直到没有任务。
    fn run(&mut self) -> Result<Ran, Fault>;
}

/// [`ITaskPoll::add`] 退回的任务。
pub struct Refused<'a>(pub Task<'a>);

impl fmt::Debug for Refused<'_> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("Refused: 不可递归的任务不能塞新任务")
    }
}

/// [`ITaskPoll::run`] 停下的原因。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Fault {
    /// 任务执行时又调了 [`ITaskPoll::run`]
    Reentrant,
    /// 不可递归的任务返回了 [`Flow::Again`]：这个任务已丢掉，所在那一轮照常跑完、新塞的任务都留在队列里，可以再 [`ITaskPoll::run`]
    NotRecursable,
    /// 剩下的任务都在等还没做完的任务（循环依赖或依赖自己），一个也跑不了；它们都留在队列里
    Deadlock,
}

/// [`ITaskPoll::run`] 跑了多少。
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Ran {
    /// 轮数
    pub rounds: usize,
    /// 执行了几次 [`ITask::exec`]
    pub execs: usize,
}

/// [`ITaskPoll`] 的实现：下一轮要跑的任务、发编号的计数器，加上跑它们的 [`Executor`]。
pub struct TaskQueue<'x, 'a, E: Executor> {
    executor: &'x E,
    /// 下一轮要跑的（含还在等依赖的），按塞进来的次序
    pending: Vec<Entry<'a>>,
    /// 下一个编号；同一轮的任务在不同线程上塞新任务，所以是原子的
    ids: AtomicUsize,
}

impl<'x, E: Executor> From<&'x E> for TaskQueue<'x, '_, E> {
    fn from(executor: &'x E) -> Self {
        Self { executor, pending: Vec::new(), ids: AtomicUsize::new(0) }
    }
}

impl<E: Executor> TaskQueue<'_, '_, E> {
    /// 没有待跑的任务。
    pub fn is_empty(&self) -> bool {
        self.pending.is_empty()
    }
}

impl<'a, E: Executor> ITaskPoll<'a> for TaskQueue<'_, 'a, E> {
    /// 宿主在循环外塞，总是收下。
    fn add(&mut self, task: Task<'a>) -> Result<TaskId, Refused<'a>> {
        let entry = Entry::new(task, &self.ids);
        let id = entry.id;
        self.pending.push(entry);
        Ok(id)
    }

    fn run(&mut self) -> Result<Ran, Fault> {
        let mut ran = Ran::default();
        let mut fault = None;
        while fault.is_none() && !self.pending.is_empty() {
            let live: BTreeSet<TaskId> = self.pending.iter().map(|e| e.id).collect();
            let ready: Vec<bool> = self.pending.iter().map(|e| e.ready(&live)).collect();
            if !ready.contains(&true) {
                fault = Some(Fault::Deadlock);
                break;
            }
            let (go, wait): (Vec<_>, Vec<_>) = mem::take(&mut self.pending).into_iter().zip(&ready).partition(|(_, r)| **r);
            let round: Vec<Job<'_, 'a>> = go.into_iter().map(|(entry, _)| Job::new(entry, &self.ids)).collect();
            ran.rounds += 1;
            ran.execs += round.len();
            let mut done = self.executor.execute(round).into_iter();
            let mut wait = wait.into_iter().map(|(entry, _)| entry);
            // 按原来的次序并回去：等着的留在原位，跑过的换成它新塞的和它自己
            for &r in &ready {
                if r {
                    fault = fault.or(done.next().and_then(|d| d.settle(&mut self.pending).err()));
                } else {
                    self.pending.extend(wait.next());
                }
            }
        }
        fault.map_or(Ok(ran), Err)
    }
}

/// 队列里的一项：编号加任务。
struct Entry<'a> {
    id: TaskId,
    task: Task<'a>,
}

impl<'a> Entry<'a> {
    /// 从 `ids` 领一个编号。
    fn new(task: Task<'a>, ids: &AtomicUsize) -> Self {
        Self { id: TaskId(ids.fetch_add(1, Ordering::Relaxed)), task }
    }

    /// 依赖的任务都不在 `live`（队列里还没做完的）里了。
    fn ready(&self, live: &BTreeSet<TaskId>) -> bool {
        self.task.depend().iter().all(|d| !live.contains(d))
    }
}

/// 一轮里的一个任务：交给 [`Executor`] 执行，带着自己的收集器。
struct Job<'q, 'a> {
    entry: Entry<'a>,
    spawn: Spawn<'q, 'a>,
}

impl<'q, 'a> Job<'q, 'a> {
    /// 新塞的任务从 `ids` 领编号。
    fn new(entry: Entry<'a>, ids: &'q AtomicUsize) -> Self {
        let spawn = Spawn { recursable: entry.task.recursable(), ids, added: Vec::new() };
        Self { entry, spawn }
    }
}

impl<'t, 'q: 't, 'a: 't> Execute<'t> for Job<'q, 'a> {
    type Output = Done<'q, 'a>;

    fn execute(mut self) -> Done<'q, 'a> {
        let flow = self.entry.task.exec(&mut self.spawn);
        Done { job: self, flow }
    }
}

/// 执行完的任务与它的去向。
struct Done<'q, 'a> {
    job: Job<'q, 'a>,
    flow: Flow,
}

impl<'a> Done<'_, 'a> {
    /// 并进下一轮：先是它新塞的，再是它自己（[`Flow::Again`]，编号不变）。
    fn settle(self, pending: &mut Vec<Entry<'a>>) -> Result<(), Fault> {
        let Job { entry, spawn } = self.job;
        pending.extend(spawn.added);
        match (self.flow, spawn.recursable) {
            (Flow::Done, _) => Ok(()),
            (Flow::Again, true) => {
                pending.push(entry);
                Ok(())
            }
            (Flow::Again, false) => Err(Fault::NotRecursable),
        }
    }
}

/// 任务执行时拿到的 [`ITaskPoll`]：新塞的任务先记在这里，整轮跑完再并进队列。
struct Spawn<'q, 'a> {
    /// 正在执行的任务能否递归
    recursable: bool,
    /// 队列的编号计数器
    ids: &'q AtomicUsize,
    added: Vec<Entry<'a>>,
}

impl<'a> ITaskPoll<'a> for Spawn<'_, 'a> {
    fn add(&mut self, task: Task<'a>) -> Result<TaskId, Refused<'a>> {
        if !self.recursable {
            return Err(Refused(task));
        }
        let entry = Entry::new(task, self.ids);
        let id = entry.id;
        self.added.push(entry);
        Ok(id)
    }

    fn run(&mut self) -> Result<Ran, Fault> {
        Err(Fault::Reentrant)
    }
}
