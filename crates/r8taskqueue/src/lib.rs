//! r8taskqueue：任务队列与事件循环（`no_std` + `alloc`）。
//!
//! - [`ITask`]：一个任务。[`ITask::recursable`] 说它能不能在执行时往队列里塞新任务（或要求下一轮再跑自己），[`ITask::exec`] 执行它
//! - [`ITaskPoll`]：队列。[`ITaskPoll::add`] 塞一个任务，[`ITaskPoll::run`] 启动事件循环，直到队列空
//! - [`TaskQueue`]：[`ITaskPoll`] 的实现
//!
//! 设计：
//! - **单线程、不加锁**：独占靠 `&mut`，任务不要求 `Send`，可以借用调用方数据（生命周期 `'a`，没有 `'static`）
//! - **按轮推进**：一轮开始时把待跑的任务整批拿走、依次执行；这一轮里新塞的任务（和要求再跑的任务）进下一轮。每一轮都必定跑完，
//!   一个任务不断塞新任务也不会把同一轮拖成死循环，只会让轮数一直涨
//! - **没有时间概念**：不排延时、不睡眠等待；队列空了 [`ITaskPoll::run`] 就返回，什么时候再跑由宿主决定
//! - **谁能塞任务**：宿主在循环外随时能塞；循环里只有正在执行的可递归任务能塞，不可递归的任务塞会被退回（[`Refused`] 带回原任务）
//! - **不重入**：任务执行时调 [`ITaskPoll::run`] 得到 [`Fault::Reentrant`]
//!
//! ```
//! use r8taskqueue::{Flow, ITask, ITaskPoll, TaskQueue};
//!
//! /// 往 `log` 里记一笔；`fanout` 不为零时再塞一个自己的下一级
//! struct Note<'a> { log: &'a core::cell::RefCell<Vec<u32>>, id: u32, fanout: u32 }
//!
//! impl<'a> ITask<'a> for Note<'a> {
//!     fn recursable(&self) -> bool { self.fanout != 0 }
//!     fn exec(&mut self, poll: &mut dyn ITaskPoll<'a>) -> Flow {
//!         self.log.borrow_mut().push(self.id);
//!         if self.fanout != 0 {
//!             let child = Note { log: self.log, id: self.id * 10, fanout: self.fanout - 1 };
//!             assert!(poll.add(Box::new(child)).is_ok());
//!         }
//!         Flow::Done
//!     }
//! }
//!
//! let log = core::cell::RefCell::new(Vec::new());
//! let mut queue = TaskQueue::default();
//! assert!(queue.add(Box::new(Note { log: &log, id: 1, fanout: 2 })).is_ok());
//! assert!(queue.add(Box::new(Note { log: &log, id: 2, fanout: 0 })).is_ok());
//! let ran = queue.run().unwrap();
//! assert_eq!(*log.borrow(), [1, 2, 10, 100]);
//! assert_eq!((ran.rounds, ran.execs), (3, 4));
//! ```
#![no_std]
extern crate alloc;

use alloc::boxed::Box;
use alloc::vec::Vec;
use core::fmt;
use core::mem;

/// 队列里存的任务：借用调用方数据，活不过 `'a`。
pub type Task<'a> = Box<dyn ITask<'a> + 'a>;

/// 一个任务。
pub trait ITask<'a> {
    /// 能否在执行时往队列里塞新任务、能否返回 [`Flow::Again`]。
    fn recursable(&self) -> bool;

    /// 执行一次。`poll` 是正在跑它的队列，可递归的任务经它塞新任务（进下一轮）。
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

/// [`ITaskPoll::run`] 中途停下的原因；还没跑的任务都留在队列里，可以再 [`ITaskPoll::run`]。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Fault {
    /// 任务执行时又调了 [`ITaskPoll::run`]
    Reentrant,
    /// 不可递归的任务返回了 [`Flow::Again`]（这个任务已丢掉）
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

/// 队列此刻在做什么。
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
enum Phase {
    /// 在循环外
    #[default]
    Idle,
    /// 正在执行一个可递归的任务
    Recursable,
    /// 正在执行一个不可递归的任务
    Leaf,
}

impl From<bool> for Phase {
    /// 正在执行的任务能否递归
    fn from(recursable: bool) -> Self {
        if recursable { Self::Recursable } else { Self::Leaf }
    }
}

/// [`ITaskPoll`] 的实现：两个 `Vec` 轮换，一个是下一轮要跑的，一个是这一轮正在跑的（空闲时留着容量复用）。
#[derive(Default)]
pub struct TaskQueue<'a> {
    /// 下一轮要跑的
    pending: Vec<Task<'a>>,
    /// 这一轮的批（循环外是空的，只留容量）
    batch: Vec<Task<'a>>,
    phase: Phase,
}

impl TaskQueue<'_> {
    /// 没有待跑的任务。
    pub fn is_empty(&self) -> bool {
        self.pending.is_empty()
    }
}

impl<'a> ITaskPoll<'a> for TaskQueue<'a> {
    fn add(&mut self, task: Task<'a>) -> Result<(), Refused<'a>> {
        match self.phase {
            Phase::Leaf => Err(Refused(task)),
            Phase::Idle | Phase::Recursable => {
                self.pending.push(task);
                Ok(())
            }
        }
    }

    fn run(&mut self) -> Result<Ran, Fault> {
        if self.phase != Phase::Idle {
            return Err(Fault::Reentrant);
        }
        let mut ran = Ran::default();
        let mut batch = mem::take(&mut self.batch);
        loop {
            mem::swap(&mut batch, &mut self.pending);
            if batch.is_empty() {
                break;
            }
            ran.rounds += 1;
            let mut tasks = batch.drain(..);
            while let Some(mut task) = tasks.next() {
                let recursable = task.recursable();
                self.phase = recursable.into();
                let flow = task.exec(self);
                self.phase = Phase::Idle;
                ran.execs += 1;
                match (flow, recursable) {
                    (Flow::Done, _) => {}
                    (Flow::Again, true) => self.pending.push(task),
                    (Flow::Again, false) => {
                        // 这一轮剩下的排在下一轮前面，再 run 时次序不变
                        self.pending.splice(0..0, tasks);
                        self.batch = batch;
                        return Err(Fault::NotRecursable);
                    }
                }
            }
        }
        self.batch = batch;
        Ok(ran)
    }
}
