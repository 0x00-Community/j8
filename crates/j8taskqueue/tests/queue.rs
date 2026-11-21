use std::num::NonZeroUsize;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::mpsc;
use std::time::Duration;

use j8taskqueue::{Fault, Flow, ITask, ITaskPoll, Pool, Quota, Stop, Task, TaskId, TaskQueue};

struct Threads(usize);

impl Pool for Threads {
    fn threads(&self) -> usize {
        self.0
    }

    fn run(&self, jobs: Vec<j8taskqueue::Job<'_>>) {
        std::thread::scope(|s| {
            for job in jobs {
                s.spawn(job);
            }
        });
    }
}

const RENDEZVOUS: Duration = Duration::from_secs(5);

const THREADS: NonZeroUsize = NonZeroUsize::new(4).unwrap();

type Body<'a> = Box<dyn FnMut(&mut dyn ITaskPoll<'a>, &mut Quota) -> Flow + Send + 'a>;

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

#[test]
fn independent_tasks_run_in_parallel() {
    let pool = Threads(THREADS.get());
    let (to_a, from_b) = mpsc::channel();
    let (to_b, from_a) = mpsc::channel();
    let (done, seen) = (AtomicUsize::new(0), AtomicUsize::new(usize::MAX));
    let (done, seen) = (&done, &seen);
    let mut q = TaskQueue::from(&pool);
    let a = q
        .add(Job::leaf(vec![], move |_, _| {
            to_b.send(()).unwrap();
            from_b.recv_timeout(RENDEZVOUS).expect("b did not run in parallel with a");
            done.fetch_add(1, Ordering::AcqRel);
            Flow::Done
        }))
        .unwrap();
    let b = q
        .add(Job::leaf(vec![], move |_, _| {
            to_a.send(()).unwrap();
            from_a.recv_timeout(RENDEZVOUS).expect("a did not run in parallel with b");
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

#[test]
fn no_barrier_between_unrelated_tasks() {
    let pool = Threads(THREADS.get());
    let (signal, wait) = mpsc::channel();
    let mut q = TaskQueue::from(&pool);
    q.add(Job::leaf(vec![], move |_, _| {
        wait.recv_timeout(RENDEZVOUS).expect("c waited for an unrelated slow task");
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

#[test]
fn chain_runs_in_order() {
    const LEN: usize = 200;
    let pool = Threads(THREADS.get());
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

#[test]
fn random_dag_respects_dependencies() {
    const TASKS: usize = 2000;
    const LCG: (u64, u64) = (6_364_136_223_846_793_005, 1_442_695_040_888_963_407);
    let pool = Threads(THREADS.get());
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
                assert!(deps.iter().all(|&d| done[d].load(Ordering::Acquire)), "{i} ran before its dependencies finished");
                done[i].store(true, Ordering::Release);
                Flow::Done
            }))
            .unwrap(),
        );
    }
    assert_eq!(q.run().unwrap().execs, TASKS);
    assert!(done.iter().all(|d| d.load(Ordering::Acquire)));
}

#[test]
fn spawned_tasks_join_family_and_dataflow() {
    const CHILDREN: usize = 20;
    let pool = Threads(THREADS.get());
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

#[test]
fn quota_and_budget() {
    fn work<'a>(left: &'a AtomicUsize) -> impl FnMut(&mut dyn ITaskPoll<'a>, &mut Quota) -> Flow + Send + 'a {
        move |_, quota| {
            let want = left.load(Ordering::Acquire);
            left.fetch_sub(quota.spend(want), Ordering::AcqRel);
            if left.load(Ordering::Acquire) == 0 { Flow::Done } else { Flow::Yield }
        }
    }
    let slice = NonZeroUsize::new(10).unwrap();

    let left = AtomicUsize::new(25);
    let mut q = TaskQueue::from(&Threads(1)).with_slice(slice);
    q.add(Job::leaf(vec![], work(&left))).unwrap();
    let ran = q.run().unwrap();
    assert_eq!((ran.execs, ran.spent, ran.stop), (3, 25, Stop::Empty));

    let left = AtomicUsize::new(25);
    let mut q = TaskQueue::from(&Threads(1)).with_slice(slice).with_budget(NonZeroUsize::new(15).unwrap());
    q.add(Job::leaf(vec![], work(&left))).unwrap();
    let ran = q.run().unwrap();
    assert_eq!((ran.execs, ran.spent, ran.stop), (2, 15, Stop::Budget));
    let ran = q.run().unwrap();
    assert_eq!((ran.execs, ran.spent, ran.stop), (1, 10, Stop::Empty));
    assert!(q.is_empty());
}

#[test]
fn faults() {
    let refused = AtomicBool::new(false);
    let reentrant = AtomicBool::new(false);
    let mut q = TaskQueue::from(&Threads(1));
    q.add(Job::leaf(vec![], |poll, _| {
        refused.store(poll.add(Job::leaf(vec![], |_, _| Flow::Done)).is_err(), Ordering::Release);
        Flow::Again
    }))
    .unwrap();
    assert_eq!(q.run(), Err(Fault::NotRecursable));
    assert!(refused.load(Ordering::Acquire));

    let mut q = TaskQueue::from(&Threads(1));
    q.add(Job::leaf(vec![], |_, _| Flow::Yield)).unwrap();
    assert_eq!(q.run(), Err(Fault::Unspent));

    let mut q = TaskQueue::from(&Threads(1));
    q.add(Job::leaf(vec![], |poll, _| {
        reentrant.store(poll.run() == Err(Fault::Reentrant), Ordering::Release);
        Flow::Done
    }))
    .unwrap();
    assert_eq!(q.run().unwrap().execs, 1);
    assert!(reentrant.load(Ordering::Acquire));
}

#[test]
fn family_find_and_cancel() {
    let pool = Threads(THREADS.get());
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
