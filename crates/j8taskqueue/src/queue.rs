//! 数据流调度：线程池里每个线程跑一个取活循环（取任务 ⇒ 执行 ⇒ 放行下游），没有轮次屏障。
//! 取不到活的线程直接离开、不自旋：之后才就绪的任务（放行的下游、让出的、塞的）由让它就绪的线程接着取，
//! 那个线程还在循环里；离开的线程回到线程池，可以帮任务里嵌套的并行（内建函数、JIT 编译）。
use alloc::boxed::Box;
use alloc::sync::Arc;
use alloc::vec::Vec;
use core::num::NonZeroUsize;
use core::sync::atomic::{AtomicBool, AtomicU8, AtomicUsize, Ordering};

use j8lockfree::Fifo;

use crate::node::{Node, TaskId};
use crate::{Fault, Flow, ITaskPoll, Job, Pool, Quota, Ran, Refused, Stop, Task};

/// 没有故障时 `fault` 的值
const NO_FAULT: u8 = 0;

/// 工作线程共用的状态，全是原子量。
struct Shared<'a> {
    /// 就绪的任务（依赖都做完了）
    ready: Fifo<TaskId<'a>>,
    /// 下一个序号
    seq: AtomicUsize,
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

impl Worker<'_, '_> {
    fn work(self) {
        let s = self.0;
        while !s.halt.load(Ordering::Acquire) {
            let Some(quota) = s.quota() else { return };
            // 取不到就离开：别的线程手上的任务之后放出来的活，由那个线程自己接着取
            let Some(id) = s.ready.pop() else { return };
            s.step(id, quota);
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

/// 不带线程的队列：就绪队列与调度状态。宿主可以长期持有、随宿主搬动；[`Queue::run`] 在调用线程上跑，
/// [`Queue::run_on`] 铺到线程池上并行跑。
pub struct Queue<'a> {
    shared: Shared<'a>,
}

impl Default for Queue<'_> {
    /// 配额与预算都不限。
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
    /// 每次 exec 最多花多少条字节码（用完返回 [`Flow::Yield`]，回就绪队列排队，先轮到别的就绪任务）。
    pub fn set_slice(&mut self, slice: NonZeroUsize) {
        self.shared.slice = slice.get();
    }

    /// 每次 run 总共最多花多少条字节码，花完返回 [`Stop::Budget`]；并行执行时最多超出每个线程一份配额。
    pub fn set_budget(&mut self, budget: NonZeroUsize) {
        self.shared.budget = budget.get();
    }

    /// 宿主在循环外塞，总是收下。
    pub fn add(&mut self, task: Task<'a>) -> TaskId<'a> {
        self.shared.register(task, None)
    }

    /// 没有待跑的任务（循环外：还在等依赖的任务，其依赖链终会落到一个就绪的任务上）。
    pub fn is_empty(&self) -> bool {
        self.shared.ready.is_empty()
    }

    /// 在调用线程上跑到没有任务、或预算花完、或出故障（同 [`ITaskPoll::run`]）。
    pub fn run(&mut self) -> Result<Ran, Fault> {
        self.begin();
        Worker(&self.shared).work();
        self.end()
    }

    /// 在 `pool` 的每个线程上各跑一个取活循环，跑到没有任务、或预算花完、或出故障（同 [`ITaskPoll::run`]）。
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
    /// 剩下的任务都做完（不执行）：依赖链互相持有句柄，逐个封口放行才能都释放。
    fn drop(&mut self) {
        while let Some(id) = self.shared.ready.pop() {
            // SAFETY: 独占；弹出的节点执行权在这里，只做完一次
            unsafe { id.0.complete(&self.shared.ready) };
        }
    }
}

/// [`ITaskPoll`] 的实现：一个 [`Queue`]，加上跑它的线程池。
pub struct TaskQueue<'x, 'a> {
    pool: &'x dyn Pool,
    queue: Queue<'a>,
}

impl<'x, P: Pool + 'x> From<&'x P> for TaskQueue<'x, '_> {
    /// 配额与预算都不限。
    fn from(pool: &'x P) -> Self {
        Self { pool, queue: Queue::default() }
    }
}

impl TaskQueue<'_, '_> {
    /// 每次 exec 最多花多少条字节码（用完返回 [`Flow::Yield`]，之后接着跑）。
    #[must_use]
    pub fn with_slice(mut self, slice: NonZeroUsize) -> Self {
        self.queue.set_slice(slice);
        self
    }

    /// 每次 [`ITaskPoll::run`] 总共最多花多少条字节码，花完返回 [`Stop::Budget`]；并行执行时最多超出每个线程一份配额。
    #[must_use]
    pub fn with_budget(mut self, budget: NonZeroUsize) -> Self {
        self.queue.set_budget(budget);
        self
    }

    /// 没有待跑的任务（循环外：还在等依赖的任务，其依赖链终会落到一个就绪的任务上）。
    pub fn is_empty(&self) -> bool {
        self.queue.is_empty()
    }
}

impl<'a> ITaskPoll<'a> for TaskQueue<'_, 'a> {
    /// 宿主在循环外塞，总是收下。
    fn add(&mut self, task: Task<'a>) -> Result<TaskId<'a>, Refused<'a>> {
        Ok(self.queue.add(task))
    }

    fn run(&mut self) -> Result<Ran, Fault> {
        self.queue.run_on(self.pool)
    }
}
