//! j8taskqueue 的调度语义：并行、无屏障、依赖次序、派生、配额、故障、家族。
use std::num::NonZeroUsize;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::mpsc;
use std::time::Duration;

use moremath::{ThreadMulti, ThreadSingle};
use j8taskqueue::{Fault, Flow, ITask, ITaskPoll, Quota, Stop, Task, TaskId, TaskQueue};

/// 等别的线程的最长时间：串行执行会在这里超时失败
const RENDEZVOUS: Duration = Duration::from_secs(5);
/// 并行用例的线程数
const THREADS: NonZeroUsize = NonZeroUsize::new(4).unwrap();

type Body<'a> = Box<dyn FnMut(&mut dyn ITaskPoll<'a>, &mut Quota) -> Flow + Send + 'a>;

/// 测试用任务：依赖表 + 闭包。
struct Job<'a> {
    after: Vec<TaskId<'a>>,
    recursable: bool,
    body: Body<'a>,
}

impl<'a> Job<'a> {
    fn leaf(after: Vec<TaskId<'a>>, body: impl FnMut(&mut dyn ITaskPoll<'a>, &mut Quota) -> Flow + Send + 'a) -> Task<'a> {
        Box::new(Self { after, recursable: false, body: Box::new(body) })
    }

    fn recursable(after: Vec<TaskId<'a>>, body: impl FnMut(&mut dyn ITaskPoll<'a>, &mut Quota) -> Flow + Send + 'a) -> Task<'a> {
        Box::new(Self { after, recursable: true, body: Box::new(body) })
    }
}

impl<'a> ITask<'a> for Job<'a> {
    fn recursable(&self) -> bool {
        self.recursable
    }

    fn depend(&self) -> &[TaskId<'a>] {
        &self.after
    }

    fn exec(&mut self, poll: &mut dyn ITaskPoll<'a>, quota: &mut Quota) -> Flow {
        (self.body)(poll, quota)
    }
}

/// a、b 都被 c 依赖、彼此独立：a、b 必须同时在跑（互相等对方的消息），c 在两者之后。
#[test]
fn independent_tasks_run_in_parallel() {
    let pool = ThreadMulti::from(THREADS);
    let (to_a, from_b) = mpsc::channel();
    let (to_b, from_a) = mpsc::channel();
    let (done, seen) = (AtomicUsize::new(0), AtomicUsize::new(usize::MAX));
    let (done, seen) = (&done, &seen);
    let mut q = TaskQueue::from(&pool);
    let a = q
        .add(Job::leaf(vec![], move |_, _| {
            to_b.send(()).unwrap();
            from_b.recv_timeout(RENDEZVOUS).expect("b 没有和 a 并行");
            done.fetch_add(1, Ordering::AcqRel);
            Flow::Done
        }))
        .unwrap();
    let b = q
        .add(Job::leaf(vec![], move |_, _| {
            to_a.send(()).unwrap();
            from_a.recv_timeout(RENDEZVOUS).expect("a 没有和 b 并行");
            done.fetch_add(1, Ordering::AcqRel);
            Flow::Done
        }))
        .unwrap();
    q.add(Job::leaf(vec![a, b], |_, _| {
        seen.store(done.load(Ordering::Acquire), Ordering::Release);
        Flow::Done
    }))
    .unwrap();
    assert_eq!(q.run().unwrap().execs, 3);
    assert_eq!(seen.load(Ordering::Acquire), 2);
}

/// 没有轮次屏障：慢任务 s 还在跑时，a ⇒ c 就能做完（s 等 c 的消息才结束）。
#[test]
fn no_barrier_between_unrelated_tasks() {
    let pool = ThreadMulti::from(THREADS);
    let (signal, wait) = mpsc::channel();
    let mut q = TaskQueue::from(&pool);
    q.add(Job::leaf(vec![], move |_, _| {
        wait.recv_timeout(RENDEZVOUS).expect("c 等了不相干的慢任务");
        Flow::Done
    }))
    .unwrap();
    let a = q.add(Job::leaf(vec![], |_, _| Flow::Done)).unwrap();
    q.add(Job::leaf(vec![a], move |_, _| {
        signal.send(()).unwrap();
        Flow::Done
    }))
    .unwrap();
    assert_eq!(q.run().unwrap().execs, 3);
}

/// 依赖链在线程池上也严格按链上次序跑。
#[test]
fn chain_runs_in_order() {
    const LEN: usize = 200;
    let pool = ThreadMulti::from(THREADS);
    let next = AtomicUsize::new(0);
    let mut q = TaskQueue::from(&pool);
    let mut prev = vec![];
    for i in 0..LEN {
        let next = &next;
        let id = q
            .add(Job::leaf(prev, move |_, _| {
                assert_eq!(next.fetch_add(1, Ordering::AcqRel), i);
                Flow::Done
            }))
            .unwrap();
        prev = vec![id];
    }
    assert_eq!(q.run().unwrap().execs, LEN);
    assert_eq!(next.load(Ordering::Acquire), LEN);
}

/// 随机依赖图：每个任务跑时它的依赖都已做完，全部任务都跑到。
#[test]
fn random_dag_respects_dependencies() {
    const TASKS: usize = 2000;
    /// 线性同余的乘数与增量
    const LCG: (u64, u64) = (6_364_136_223_846_793_005, 1_442_695_040_888_963_407);
    let pool = ThreadMulti::from(THREADS);
    let done: Vec<AtomicBool> = (0..TASKS).map(|_| AtomicBool::new(false)).collect();
    let mut q = TaskQueue::from(&pool);
    let mut ids: Vec<TaskId<'_>> = Vec::new();
    let mut rng = 1_u64;
    for i in 0..TASKS {
        let mut deps = Vec::new();
        for _ in 0..3 {
            rng = rng.wrapping_mul(LCG.0).wrapping_add(LCG.1);
            if i > 0 {
                deps.push(usize::try_from(rng >> 33).unwrap() % i);
            }
        }
        let after = deps.iter().map(|&d| ids[d].clone()).collect();
        let done = &done;
        ids.push(
            q.add(Job::leaf(after, move |_, _| {
                assert!(deps.iter().all(|&d| done[d].load(Ordering::Acquire)), "{i} 跑时依赖没做完");
                done[i].store(true, Ordering::Release);
                Flow::Done
            }))
            .unwrap(),
        );
    }
    assert_eq!(q.run().unwrap().execs, TASKS);
    assert!(done.iter().all(|d| d.load(Ordering::Acquire)));
}

/// 可递归任务派生：子任务当场登记、并行跑；收尾任务依赖全部子任务，并进同一家族。
#[test]
fn spawned_tasks_join_family_and_dataflow() {
    const CHILDREN: usize = 20;
    let pool = ThreadMulti::from(THREADS);
    let count = AtomicUsize::new(0);
    let seen = AtomicUsize::new(0);
    let family = std::sync::OnceLock::new();
    let mut q = TaskQueue::from(&pool);
    let root = q
        .add(Job::recursable(vec![], |poll, _| {
            let kids: Vec<TaskId<'_>> = (0..CHILDREN)
                .map(|_| {
                    poll.add(Job::leaf(vec![], |_, _| {
                        count.fetch_add(1, Ordering::AcqRel);
                        Flow::Done
                    }))
                    .unwrap()
                })
                .collect();
            let last = poll
                .add(Job::leaf(kids, |_, _| {
                    seen.store(count.load(Ordering::Acquire), Ordering::Release);
                    Flow::Done
                }))
                .unwrap();
            family.set(format!("{:?}", last.family())).unwrap();
            Flow::Done
        }))
        .unwrap();
    assert_eq!(q.run().unwrap().execs, CHILDREN + 2);
    assert_eq!(seen.load(Ordering::Acquire), CHILDREN);
    assert_eq!(family.get(), Some(&format!("{root:?}")));
}

/// 配额：25 条字节码的活、每次 10 条 ⇒ 三次 exec；预算 15 ⇒ 第一次 run 停在预算上，第二次跑完。
#[test]
fn quota_and_budget() {
    /// 剩 `left` 条字节码的活：每次花掉配额能给的，花完算做完
    fn work<'a>(left: &'a AtomicUsize) -> impl FnMut(&mut dyn ITaskPoll<'a>, &mut Quota) -> Flow + Send + 'a {
        move |_, quota| {
            let want = left.load(Ordering::Acquire);
            left.fetch_sub(quota.spend(want), Ordering::AcqRel);
            if left.load(Ordering::Acquire) == 0 { Flow::Done } else { Flow::Yield }
        }
    }
    let slice = NonZeroUsize::new(10).unwrap();

    let left = AtomicUsize::new(25);
    let mut q = TaskQueue::from(&ThreadSingle).with_slice(slice);
    q.add(Job::leaf(vec![], work(&left))).unwrap();
    let ran = q.run().unwrap();
    assert_eq!((ran.execs, ran.spent, ran.stop), (3, 25, Stop::Empty));

    let left = AtomicUsize::new(25);
    let mut q = TaskQueue::from(&ThreadSingle).with_slice(slice).with_budget(NonZeroUsize::new(15).unwrap());
    q.add(Job::leaf(vec![], work(&left))).unwrap();
    let ran = q.run().unwrap();
    assert_eq!((ran.execs, ran.spent, ran.stop), (2, 15, Stop::Budget));
    let ran = q.run().unwrap();
    assert_eq!((ran.execs, ran.spent, ran.stop), (1, 10, Stop::Empty));
    assert!(q.is_empty());
}

/// 故障：不可递归的任务塞新任务被退回、返回 Again 报 NotRecursable；配额没用完返回 Yield 报 Unspent；循环里再 run 报 Reentrant。
#[test]
fn faults() {
    let refused = AtomicBool::new(false);
    let reentrant = AtomicBool::new(false);
    let mut q = TaskQueue::from(&ThreadSingle);
    q.add(Job::leaf(vec![], |poll, _| {
        refused.store(poll.add(Job::leaf(vec![], |_, _| Flow::Done)).is_err(), Ordering::Release);
        Flow::Again
    }))
    .unwrap();
    assert_eq!(q.run(), Err(Fault::NotRecursable));
    assert!(refused.load(Ordering::Acquire));

    let mut q = TaskQueue::from(&ThreadSingle);
    q.add(Job::leaf(vec![], |_, _| Flow::Yield)).unwrap();
    assert_eq!(q.run(), Err(Fault::Unspent));

    let mut q = TaskQueue::from(&ThreadSingle);
    q.add(Job::leaf(vec![], |poll, _| {
        reentrant.store(poll.run() == Err(Fault::Reentrant), Ordering::Release);
        Flow::Done
    }))
    .unwrap();
    assert_eq!(q.run().unwrap().execs, 1);
    assert!(reentrant.load(Ordering::Acquire));
}

/// 家族：依赖把任务并成一家、爹是最早的；取消一家只影响这一家。
#[test]
fn family_find_and_cancel() {
    let pool = ThreadMulti::from(THREADS);
    let ran = AtomicUsize::new(0);
    let tick = || {
        |_: &mut dyn ITaskPoll<'_>, _: &mut Quota| {
            ran.fetch_add(1, Ordering::AcqRel);
            Flow::Done
        }
    };
    let mut q = TaskQueue::from(&pool);
    let x1 = q.add(Job::leaf(vec![], tick())).unwrap();
    let y1 = q.add(Job::leaf(vec![], tick())).unwrap();
    let x2 = q.add(Job::leaf(vec![x1.clone()], tick())).unwrap();
    let y2 = q.add(Job::leaf(vec![y1.clone()], tick())).unwrap();
    let x3 = q.add(Job::leaf(vec![x2.clone()], tick())).unwrap();
    assert_eq!(x3.family(), x1);
    assert_eq!(y2.family(), y1);
    assert_ne!(x3.family(), y2.family());
    x2.cancel();
    assert_eq!(q.run().unwrap().execs, 2);
    assert_eq!(ran.load(Ordering::Acquire), 2);
}
