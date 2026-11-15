//! 数据流调度：线程池里每个线程跑一个取活循环（取任务 ⇒ 执行 ⇒ 放行下游），没有轮次屏障。
use alloc::sync::Arc;
use alloc::vec::Vec;
use core::hint;
use core::num::NonZeroUsize;
use core::sync::atomic::{AtomicBool, AtomicU8, AtomicUsize, Ordering};

use moremath::{Execute, Executor};
use r8lockfree::Fifo;

use crate::node::{Node, TaskId};
use crate::{Fault, Flow, ITaskPoll, Quota, Ran, Refused, Stop, Task};

/// 没有故障时 `fault` 的值
const NO_FAULT: u8 = 0;

/// 工作线程共用的状态，全是原子量。
struct Shared<'a> {
    /// 就绪的任务（依赖都做完了）
    ready: Fifo<TaskId<'a>>,
    /// 下一个序号
    seq: AtomicUsize,
    /// 正在取活 / 执行的工作线程数
    active: AtomicUsize,
    /// 这次 run 花掉的字节码条数
    spent: AtomicUsize,
    /// 这次 run 执行了几次
    execs: AtomicUsize,
    /// 第一个故障（[`Fault`] 的编码），没有是 [`NO_FAULT`]
    fault: AtomicU8,
    /// 停止取新任务（故障或预算花完）
    halt: AtomicBool,
    /// 每次 exec 的配额
    slice: usize,
    /// 每次 run 的预算
    budget: usize,
}

impl<'a> Shared<'a> {
    /// 登记一个任务：并进家族、登记到各依赖的下游表，依赖都做完了就推进就绪队列。
    fn register(&self, task: Task<'a>, spawner: Option<&TaskId<'a>>) -> TaskId<'a> {
        let id = TaskId(Arc::new(Node::new(task, self.seq.fetch_add(1, Ordering::Relaxed))));
        if let Some(p) = spawner {
            Node::union(&id.0, &p.0);
        }
        // SAFETY: 节点还没进就绪队列，只有这里碰它的任务
        for d in unsafe { id.0.depend() } {
            Node::union(&id.0, &d.0);
            id.0.hold();
            if d.0.push_succ(id.clone()).is_err() {
                id.0.release();
            }
        }
        // 撤掉登记期间的保护
        if id.0.release() {
            self.ready.push(id.clone());
        }
        id
    }

    /// 这次 exec 的配额；预算花完返回 None 并停止取新任务。
    fn quota(&self) -> Option<usize> {
        let left = self.budget.saturating_sub(self.spent.load(Ordering::Acquire));
        if left == 0 {
            self.halt.store(true, Ordering::Release);
            return None;
        }
        Some(self.slice.min(left))
    }

    /// 记下第一个故障，停止取新任务。
    fn fail(&self, fault: Fault) {
        let _ = self.fault.compare_exchange(NO_FAULT, fault.into(), Ordering::AcqRel, Ordering::Acquire);
        self.halt.store(true, Ordering::Release);
    }

    /// 执行一个刚弹出来的任务，按结果放行下游或放回就绪队列。
    fn step(&self, id: TaskId<'a>, quota: usize) {
        // SAFETY: id 刚从就绪队列弹出，执行权在这个线程
        let Some(mut task) = (unsafe { id.0.take() }) else { return };
        if id.0.cancelled() {
            drop(task);
            // SAFETY: 同上，取消的任务只在这里做完一次
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
            // SAFETY: 执行权还在这个线程，放回后才进就绪队列
            unsafe { id.0.put(task) };
            self.ready.push(id);
        } else {
            drop(task);
            // SAFETY: 执行权在这个线程，这里是它唯一一次做完
            unsafe { id.0.complete(&self.ready) };
        }
    }
}

/// 一个工作线程的取活循环。
struct Worker<'s, 'a>(&'s Shared<'a>);

impl<'t, 's: 't, 'a: 't> Execute<'t> for Worker<'s, 'a> {
    type Output = ();

    fn execute(self) {
        let s = self.0;
        // 先登记「我在取活」再弹：别的线程看到 active 为 0 且队列空，才能断定不会再有新任务
        while !s.halt.load(Ordering::Acquire) {
            let Some(quota) = s.quota() else { return };
            s.active.fetch_add(1, Ordering::SeqCst);
            match s.ready.pop() {
                Some(id) => {
                    s.step(id, quota);
                    s.active.fetch_sub(1, Ordering::SeqCst);
                }
                None => {
                    if s.active.fetch_sub(1, Ordering::SeqCst) == 1 && s.ready.is_empty() {
                        return;
                    }
                    hint::spin_loop();
                }
            }
        }
    }
}

/// 任务执行时拿到的 [`ITaskPoll`]：塞的任务当场登记，依赖都做完就能被别的线程拿走。
struct Spawn<'s, 'n, 'a> {
    shared: &'s Shared<'a>,
    /// 正在执行的任务（新任务并进它的家族）
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

/// [`ITaskPoll`] 的实现：就绪队列与调度状态，加上跑它们的 [`Executor`]。
pub struct TaskQueue<'x, 'a, E: Executor> {
    executor: &'x E,
    shared: Shared<'a>,
}

impl<'x, E: Executor> From<&'x E> for TaskQueue<'x, '_, E> {
    /// 配额与预算都不限。
    fn from(executor: &'x E) -> Self {
        let shared = Shared {
            ready: Fifo::default(),
            seq: AtomicUsize::new(0),
            active: AtomicUsize::new(0),
            spent: AtomicUsize::new(0),
            execs: AtomicUsize::new(0),
            fault: AtomicU8::new(NO_FAULT),
            halt: AtomicBool::new(false),
            slice: Quota::UNLIMITED,
            budget: Quota::UNLIMITED,
        };
        Self { executor, shared }
    }
}

impl<E: Executor> TaskQueue<'_, '_, E> {
    /// 每次 exec 最多花多少条字节码（用完返回 [`Flow::Yield`]，之后接着跑）。
    #[must_use]
    pub fn with_slice(mut self, slice: NonZeroUsize) -> Self {
        self.shared.slice = slice.get();
        self
    }

    /// 每次 [`ITaskPoll::run`] 总共最多花多少条字节码，花完返回 [`Stop::Budget`]；并行执行时最多超出每个线程一份配额。
    #[must_use]
    pub fn with_budget(mut self, budget: NonZeroUsize) -> Self {
        self.shared.budget = budget.get();
        self
    }

    /// 没有待跑的任务（循环外：还在等依赖的任务，其依赖链终会落到一个就绪的任务上）。
    pub fn is_empty(&self) -> bool {
        self.shared.ready.is_empty()
    }
}

impl<'a, E: Executor> ITaskPoll<'a> for TaskQueue<'_, 'a, E> {
    /// 宿主在循环外塞，总是收下。
    fn add(&mut self, task: Task<'a>) -> Result<TaskId<'a>, Refused<'a>> {
        Ok(self.shared.register(task, None))
    }

    fn run(&mut self) -> Result<Ran, Fault> {
        let s = &mut self.shared;
        *s.spent.get_mut() = 0;
        *s.execs.get_mut() = 0;
        *s.fault.get_mut() = NO_FAULT;
        *s.halt.get_mut() = false;
        let workers: Vec<Worker<'_, 'a>> = (0..self.executor.threads()).map(|_| Worker(&self.shared)).collect();
        self.executor.execute(workers);
        let s = &mut self.shared;
        s.ready.compact();
        if let Ok(fault) = Fault::try_from(*s.fault.get_mut()) {
            return Err(fault);
        }
        let stop = if s.ready.is_empty() { Stop::Empty } else { Stop::Budget };
        Ok(Ran { execs: *s.execs.get_mut(), spent: *s.spent.get_mut(), stop })
    }
}

impl<E: Executor> Drop for TaskQueue<'_, '_, E> {
    /// 剩下的任务都做完（不执行）：依赖链互相持有句柄，逐个封口放行才能都释放。
    fn drop(&mut self) {
        while let Some(id) = self.shared.ready.pop() {
            // SAFETY: 独占；弹出的节点执行权在这里，只做完一次
            unsafe { id.0.complete(&self.shared.ready) };
        }
    }
}
