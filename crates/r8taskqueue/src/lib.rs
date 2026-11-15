//! r8taskqueue：任务队列与事件循环（`no_std` + `alloc`），每一轮的任务在 moremath 的 [`Executor`] 上并行跑。
//!
//! - [`ITask`]：一个任务。[`ITask::recursable`] 说它能不能在执行时塞新任务（或要求下一轮再跑自己），[`ITask::exec`] 执行它
//! - [`ITaskPoll`]：队列。[`ITaskPoll::add`] 塞一个任务，[`ITaskPoll::run`] 启动事件循环，直到没有任务
//! - [`TaskQueue`]：[`ITaskPoll`] 的实现，借用一个 [`Executor`]（宿主给 `ThreadMulti` 线程池；`no_std` 下可用 `ThreadSingle`）
//!
//! 设计：
//! - **按轮推进、整轮并行**：一轮开始时把待跑的任务整批交给 [`Executor::execute`]，同一轮的任务互不等待、并行执行；
//!   这一轮里新塞的任务（和要求再跑的任务）进下一轮。每一轮必定跑完，任务不断塞新任务只会让轮数一直涨
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
#![no_std]
extern crate alloc;

use alloc::boxed::Box;
use alloc::vec::Vec;
use core::fmt;
use core::mem;

use moremath::{Execute, Executor};

/// 队列里存的任务：借用调用方数据，活不过 `'a`。
pub type Task<'a> = Box<dyn ITask<'a> + 'a>;

/// 一个任务；同一轮的任务可能在不同线程上执行，所以要 `Send`。
pub trait ITask<'a>: Send {
    /// 能否在执行时往队列里塞新任务、能否返回 [`Flow::Again`]。
    fn recursable(&self) -> bool;

    /// 执行一次。可递归的任务经 `poll` 塞新任务（进下一轮）。
    fn exec(&mut self, poll: &mut dyn ITaskPoll<'a>) -> Flow;
}

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
    /// 塞一个任务，进下一轮。循环里由不可递归的任务塞时退回。
    fn add(&mut self, task: Task<'a>) -> Result<(), Refused<'a>>;

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
}

/// [`ITaskPoll::run`] 跑了多少。
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Ran {
    /// 轮数
    pub rounds: usize,
    /// 执行了几次 [`ITask::exec`]
    pub execs: usize,
}

/// [`ITaskPoll`] 的实现：下一轮要跑的任务，加上跑它们的 [`Executor`]。
pub struct TaskQueue<'x, 'a, E: Executor> {
    executor: &'x E,
    /// 下一轮要跑的
    pending: Vec<Task<'a>>,
}

impl<'x, E: Executor> From<&'x E> for TaskQueue<'x, '_, E> {
    fn from(executor: &'x E) -> Self {
        Self { executor, pending: Vec::new() }
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
    fn add(&mut self, task: Task<'a>) -> Result<(), Refused<'a>> {
        self.pending.push(task);
        Ok(())
    }

    fn run(&mut self) -> Result<Ran, Fault> {
        let mut ran = Ran::default();
        let mut fault = None;
        while fault.is_none() && !self.pending.is_empty() {
            let round: Vec<Job<'a>> = mem::take(&mut self.pending).into_iter().map(Job::from).collect();
            ran.rounds += 1;
            ran.execs += round.len();
            for done in self.executor.execute(round) {
                fault = fault.or(done.settle(&mut self.pending).err());
            }
        }
        fault.map_or(Ok(ran), Err)
    }
}

/// 一轮里的一个任务：交给 [`Executor`] 执行，带着自己的收集器。
struct Job<'a> {
    task: Task<'a>,
    spawn: Spawn<'a>,
}

impl<'a> From<Task<'a>> for Job<'a> {
    fn from(task: Task<'a>) -> Self {
        let spawn = Spawn { recursable: task.recursable(), added: Vec::new() };
        Self { task, spawn }
    }
}

impl<'a> Execute<'a> for Job<'a> {
    type Output = Done<'a>;

    fn execute(mut self) -> Done<'a> {
        let flow = self.task.exec(&mut self.spawn);
        Done { job: self, flow }
    }
}

/// 执行完的任务与它的去向。
struct Done<'a> {
    job: Job<'a>,
    flow: Flow,
}

impl<'a> Done<'a> {
    /// 并进下一轮：先是它新塞的，再是它自己（[`Flow::Again`]）。
    fn settle(self, pending: &mut Vec<Task<'a>>) -> Result<(), Fault> {
        let Job { task, spawn } = self.job;
        pending.extend(spawn.added);
        match (self.flow, spawn.recursable) {
            (Flow::Done, _) => Ok(()),
            (Flow::Again, true) => {
                pending.push(task);
                Ok(())
            }
            (Flow::Again, false) => Err(Fault::NotRecursable),
        }
    }
}

/// 任务执行时拿到的 [`ITaskPoll`]：新塞的任务先记在这里，整轮跑完再并进队列。
struct Spawn<'a> {
    /// 正在执行的任务能否递归
    recursable: bool,
    added: Vec<Task<'a>>,
}

impl<'a> ITaskPoll<'a> for Spawn<'a> {
    fn add(&mut self, task: Task<'a>) -> Result<(), Refused<'a>> {
        if !self.recursable {
            return Err(Refused(task));
        }
        self.added.push(task);
        Ok(())
    }

    fn run(&mut self) -> Result<Ran, Fault> {
        Err(Fault::Reentrant)
    }
}
