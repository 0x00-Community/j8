//! j8taskqueue：无锁的数据流任务队列（`no_std` + `alloc`），任务在 moremath 的 [`moremath::Executor`] 上并行跑。
//!
//! - [`ITask`]：一个任务。[`ITask::recursable`] 说它能不能在执行时塞新任务、能不能要求再跑，
//!   [`ITask::depend`] 说它要等哪些任务先做完，[`ITask::exec`] 执行它（带字节码配额 [`Quota`]）
//! - [`ITaskPoll`]：队列。[`ITaskPoll::add`] 塞一个任务、拿到它的 [`TaskId`]，[`ITaskPoll::run`] 启动事件循环
//! - [`TaskQueue`]：[`ITaskPoll`] 的实现，借用一个 Executor（宿主给 `ThreadMulti` 线程池；`no_std` 下可用 `ThreadSingle`）
//! - [`TaskId`]：任务句柄；[`TaskId::family`] 找爹（并查集的根），[`TaskId::cancel`] 取消整个家族
//!
//! 设计：
//! - **数据流、无屏障**：线程池里每个线程跑取活循环；一个任务做完，依赖它的任务里等齐了的立刻进就绪队列，
//!   被还在循环里的线程拿走。a、b 都被 c 依赖而 a、b 互不依赖 ⇒ a、b 并行，两个都做完 c 马上跑，不等不相干的任务
//! - **不空转**：取不到活的线程直接离开（回线程池），不自旋；之后才就绪的活由让它就绪的线程接着跑
//! - **全程无锁**：依赖计数、下游表（j8lockfree 的可封口 PushList）、并查集父指针、就绪队列（j8lockfree 的分段 MPMC 队列 Fifo）都是原子操作
//! - **图不成环**：依赖只能写已经塞进来的任务（[`TaskId`]），依赖表在塞进来时读一次 ⇒ 不会死锁
//! - **家族**：依赖与派生（任务里塞的）把两边并成一家；家族只用于管理（找爹、整家取消），不影响并行
//! - **配额按字节码条数**：每次 exec 给一份 [`Quota`]，执行器每跑一条字节码 [`Quota::spend`]；用完没做完返回 [`Flow::Yield`]，
//!   回就绪队列排队接着跑。每次 run 还有总预算，花完返回 [`Stop::Budget`]，再 run 接着跑
//! - **借用而非 `'static`**：任务活在 `'a` 里、可借用调用方数据，只要求 `Send`
//! - **谁能塞任务**：宿主在循环外随时能塞；循环里只有可递归的任务能塞，不可递归的被退回（[`Refused`] 带回原任务）
//!
//! ```
//! use std::sync::atomic::{AtomicUsize, Ordering};
//! use moremath::ThreadMulti;
//! use j8taskqueue::{Flow, ITask, ITaskPoll, Quota, TaskId, TaskQueue};
//!
//! /// 等 `after` 都做完再跑：`done` 加一，并记下跑时已经做完了几个
//! struct Step<'a> { done: &'a AtomicUsize, seen: &'a AtomicUsize, after: Vec<TaskId<'a>> }
//!
//! impl<'a> ITask<'a> for Step<'a> {
//!     fn recursable(&self) -> bool { false }
//!     fn depend(&self) -> &[TaskId<'a>] { &self.after }
//!     fn exec(&mut self, _: &mut dyn ITaskPoll<'a>, _: &mut Quota) -> Flow {
//!         self.seen.store(self.done.load(Ordering::Acquire), Ordering::Release);
//!         self.done.fetch_add(1, Ordering::AcqRel);
//!         Flow::Done
//!     }
//! }
//!
//! let (done, seen_a, seen_b, seen_c) = (AtomicUsize::new(0), AtomicUsize::new(9), AtomicUsize::new(9), AtomicUsize::new(9));
//! let pool = ThreadMulti::default();
//! let mut queue = TaskQueue::from(&pool);
//! let a = queue.add(Box::new(Step { done: &done, seen: &seen_a, after: vec![] })).unwrap();
//! let b = queue.add(Box::new(Step { done: &done, seen: &seen_b, after: vec![] })).unwrap();
//! let c = queue.add(Box::new(Step { done: &done, seen: &seen_c, after: vec![a.clone(), b.clone()] })).unwrap();
//! assert_eq!(c.family(), a); // a、b、c 由依赖并成一家，爹是最早的 a
//! let ran = queue.run().unwrap();
//! assert_eq!(ran.execs, 3);
//! assert_eq!(seen_c.load(Ordering::Acquire), 2); // c 跑时 a、b 都做完了
//! ```
#![no_std]
extern crate alloc;

mod node;
mod queue;

use alloc::boxed::Box;
use core::fmt;

pub use node::TaskId;
pub use queue::TaskQueue;

/// 队列里存的任务：借用调用方数据，活不过 `'a`。
pub type Task<'a> = Box<dyn ITask<'a> + 'a>;

/// 一个任务；任意工作线程都可能执行它，所以要 `Send`。
pub trait ITask<'a>: Send {
    /// 能否在执行时往队列里塞新任务、能否返回 [`Flow::Again`]。
    fn recursable(&self) -> bool;

    /// 要等哪些任务先做完；塞进队列时读一次，缺省不等。
    fn depend(&self) -> &[TaskId<'a>] {
        &[]
    }

    /// 执行一次。可递归的任务经 `poll` 塞新任务；跑字节码的任务每条从 `quota` 里扣。
    fn exec(&mut self, poll: &mut dyn ITaskPoll<'a>, quota: &mut Quota) -> Flow;
}

/// 任务执行完之后怎么办。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Flow {
    /// 做完了：丢掉，放行依赖它的任务
    Done,
    /// 再跑一次（只有可递归的任务能这样）
    Again,
    /// 配额用完、还没做完：回就绪队列排队接着跑（配额没用完不能这样）
    Yield,
}

/// 一次 exec 的字节码配额。
#[derive(Debug)]
pub struct Quota {
    left: usize,
}

impl Quota {
    /// 不限
    pub const UNLIMITED: usize = usize::MAX;

    /// 还剩多少条。
    pub fn left(&self) -> usize {
        self.left
    }

    /// 花掉 `n` 条（最多花到 0），返回实际花掉的。
    pub fn spend(&mut self, n: usize) -> usize {
        let n = n.min(self.left);
        self.left -= n;
        n
    }

    /// 用完了。
    pub fn exhausted(&self) -> bool {
        self.left == 0
    }
}

impl From<usize> for Quota {
    fn from(left: usize) -> Self {
        Self { left }
    }
}

/// 任务队列。
pub trait ITaskPoll<'a> {
    /// 塞一个任务，返回它的句柄（给别的任务的 [`ITask::depend`] 用）。循环里由不可递归的任务塞时退回。
    fn add(&mut self, task: Task<'a>) -> Result<TaskId<'a>, Refused<'a>>;

    /// 启动事件循环：跑到没有任务、或预算花完、或出故障。
    fn run(&mut self) -> Result<Ran, Fault>;
}

/// [`ITaskPoll::add`] 退回的任务。
pub struct Refused<'a>(pub Task<'a>);

impl fmt::Debug for Refused<'_> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("Refused: 不可递归的任务不能塞新任务")
    }
}

/// [`ITaskPoll::run`] 因故障停下：出故障的任务已丢掉（当做完），别的线程手上的照常做完，其余任务留在队列里，可以再 run。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[repr(u8)]
pub enum Fault {
    /// 任务执行时又调了 [`ITaskPoll::run`]
    Reentrant = 1,
    /// 不可递归的任务返回了 [`Flow::Again`]
    NotRecursable = 2,
    /// 配额没用完却返回了 [`Flow::Yield`]
    Unspent = 3,
}

impl Fault {
    const ALL: [Self; 3] = [Self::Reentrant, Self::NotRecursable, Self::Unspent];
}

impl From<Fault> for u8 {
    fn from(f: Fault) -> Self {
        f as u8
    }
}

impl TryFrom<u8> for Fault {
    type Error = u8;

    fn try_from(code: u8) -> Result<Self, u8> {
        Self::ALL.into_iter().find(|f| u8::from(*f) == code).ok_or(code)
    }
}

/// [`ITaskPoll::run`] 为什么返回。
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Stop {
    /// 没有任务了
    #[default]
    Empty,
    /// 预算花完，还有任务
    Budget,
}

/// [`ITaskPoll::run`] 跑了多少。
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Ran {
    /// 执行了几次 [`ITask::exec`]
    pub execs: usize,
    /// 花掉的字节码条数
    pub spent: usize,
    pub stop: Stop,
}
