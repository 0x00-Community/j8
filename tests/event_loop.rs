//! Event loop sets: many event loops on a few pinned threads, load balancing (placement, pulling, `rebalance`,
//! `split`, moving a paused script), one quota.
use std::future::Future;
use std::num::NonZeroUsize;
use std::pin::Pin;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, OnceLock};
use std::task::{Context, Poll};
use std::time::{Duration, Instant};

use j8builtins::Builtins;
use j8executor::Executor;
use j8framework::{EventLoop, EventLoopSet, ScriptKind, Source};
use j8platform::DefaultPlatform;

/// Iterations of the long script: it needs many turns
const LONG: u32 = 1_000_000;

struct Later;

#[j8::object("later")]
impl Later {
    #[allow(clippy::unused_async, clippy::unused_async_trait_impl, reason = "an async binding that is ready at once is part of what is tested")]
    async fn double(x: f64) -> f64 {
        x * 2.0
    }
}

j8::runtime! {
    Host {
        globals: [Later],
    }
}

/// A script taking `LONG` loop iterations; its value is `seed` plus the sum of `i % 7`
fn long(seed: u32) -> String {
    format!("let n = {seed}; for (let i = 0; i < {LONG}; i++) n += i % 7; n")
}

fn long_value(seed: u32) -> String {
    (u64::from(seed) + (0..u64::from(LONG)).map(|i| i % 7).sum::<u64>()).to_string()
}

fn text(l: &mut EventLoop<'_, Executor>, g: &j8framework::Global) -> String {
    l.enter(|a| {
        let v = a.local(g);
        String::from(&**a.to_string(v).expect("ToString"))
    })
}

/// Waits (a few seconds at most) until `ok` holds
fn until(mut ok: impl FnMut() -> bool) -> bool {
    let end = Instant::now() + Duration::from_secs(5);
    while Instant::now() < end {
        if ok() {
            return true;
        }
        std::thread::sleep(Duration::from_millis(2));
    }
    ok()
}

#[test]
fn eight_threads_a_thousand_runtimes() {
    const THREADS: usize = 8;
    const RUNTIMES: usize = 1000;
    let platform = DefaultPlatform::default();
    EventLoopSet::scope(&platform, NonZeroUsize::new(THREADS).expect("threads"), |set| {
        let mut runtimes: Vec<j8::Runtime<'_>> = (0..RUNTIMES).map(|_| j8::Runtime::on::<Host>(set, Executor::default()).expect("runtime")).collect();
        assert_eq!(set.loads(), vec![RUNTIMES / THREADS; THREADS], "placed evenly");
        // One host thread per slice of the runtimes, all at once
        std::thread::scope(|s| {
            for (k, chunk) in runtimes.chunks_mut(RUNTIMES / THREADS).enumerate() {
                s.spawn(move || {
                    for (i, rt) in chunk.iter_mut().enumerate() {
                        let n = k * (RUNTIMES / THREADS) + i;
                        rt.eval(&format!("globalThis.id = {n}; later.double(id).then(v => globalThis.got = v);"), "a.js").expect("runs");
                        rt.run_until_idle().expect("settles");
                        let g = rt.eval("got + ':' + id", "check.js").expect("runs");
                        assert_eq!(rt.text(&g), format!("{}:{n}", 2 * n));
                    }
                });
            }
        });
    });
}

#[test]
fn split_moves_paused_scripts() {
    let platform = DefaultPlatform::default();
    EventLoopSet::scope(&platform, NonZeroUsize::MIN, |set| {
        let mut loops: Vec<EventLoop<'_, Executor>> = (0..4).map(|_| set.spawn(Executor::default(), &Builtins)).collect();
        let tickets: Vec<_> = loops.iter_mut().zip(0..).map(|(l, seed)| l.submit(&Source { text: &long(seed), file: "long.js" }, ScriptKind::Classic)).collect();
        // Every script is running (paused between turns) before the split
        assert!(until(|| loops.iter().all(|l| l.spent() > 0)));
        let new = set.split(0);
        for ((l, t), seed) in loops.iter_mut().zip(tickets).zip(0..) {
            let g = l.wait(t).expect("runs");
            assert_eq!(text(l, &g), long_value(seed));
        }
        // The split gives half; the new thread, idle, may steal more of those waiting
        assert!(loops.iter().filter(|l| l.thread() == new).count() >= 2, "at least half moved to the new thread");
    });
}

#[test]
fn idle_threads_pull_and_rebalance_evens_out() {
    let platform = DefaultPlatform::default();
    EventLoopSet::scope(&platform, NonZeroUsize::new(2).expect("threads"), |set| {
        let mut loops: Vec<EventLoop<'_, Executor>> = (0..20).map(|_| set.spawn(Executor::default(), &Builtins)).collect();
        assert_eq!(set.loads(), vec![10, 10]);
        set.split(0);
        // The split leaves [5, 10, 5]; the idle threads pull from the busy one
        assert!(until(|| set.loads()[1] < 10), "an idle thread pulled: {:?}", set.loads());
        set.rebalance();
        assert!(until(|| {
            let l = set.loads();
            l.iter().max().unwrap_or(&0) - l.iter().min().unwrap_or(&0) <= 1 && l.iter().sum::<usize>() == 20
        }), "evened out: {:?}", set.loads());
        // Every event loop still works where it ended up
        for (i, l) in loops.iter_mut().enumerate() {
            let g = l.eval(&Source { text: &format!("{i} * 3"), file: "x.js" }, ScriptKind::Classic).expect("runs");
            assert_eq!(text(l, &g), (i * 3).to_string());
        }
    });
}

#[test]
fn one_quota_by_weight() {
    let platform = DefaultPlatform::default();
    EventLoopSet::scope(&platform, NonZeroUsize::MIN, |set| {
        let (mut heavy, mut light) = (set.spawn(Executor::default(), &Builtins), set.spawn(Executor::default(), &Builtins));
        heavy.set_weight(NonZeroUsize::new(4).expect("weight"));
        let th = heavy.submit(&Source { text: &long(0), file: "heavy.js" }, ScriptKind::Classic);
        let mut tl = light.submit(&Source { text: &long(0), file: "light.js" }, ScriptKind::Classic);
        // Four times the share: the heavy one finishes while the light one is a quarter through
        heavy.wait(th).expect("runs");
        assert!(tl.try_take().is_none(), "the lighter event loop finished first");
        light.wait(tl).expect("runs");
        assert_eq!(set.spent().iter().sum::<u64>(), heavy.spent() + light.spent(), "steps add up");
    });
}

/// A host future that becomes ready on another thread after `after`, and records when it was first polled ready
struct Delayed {
    after: Duration,
    started: Option<Arc<AtomicBool>>,
    ready_at: Arc<OnceLock<Instant>>,
}

impl Future for Delayed {
    type Output = j8framework::Settle;

    fn poll(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Self::Output> {
        match &self.started {
            Some(done) if done.load(Ordering::Acquire) => {
                let _ = self.ready_at.set(Instant::now());
                Poll::Ready(Box::new(|_| Ok(j8framework::Value::from(7.0))))
            }
            Some(_) => Poll::Pending,
            None => {
                let (done, waker, after) = (Arc::new(AtomicBool::new(false)), cx.waker().clone(), self.after);
                let flag = Arc::clone(&done);
                std::thread::spawn(move || {
                    std::thread::sleep(after);
                    flag.store(true, Ordering::Release);
                    waker.wake();
                });
                self.started = Some(done);
                Poll::Pending
            }
        }
    }
}

#[test]
fn the_queue_is_checked_between_slices_of_a_script() {
    let platform = DefaultPlatform::default();
    let mut lp = EventLoop::new(&platform, Executor::default(), &Builtins);
    let ready_at = Arc::new(OnceLock::new());
    let delayed = Delayed { after: Duration::from_millis(20), started: None, ready_at: Arc::clone(&ready_at) };
    // A promise settled by the host future; its reaction records whether the long script had finished
    lp.enter(move |a| {
        let promise = a.new_promise();
        let (resolve, reject) = a.promise_resolvers(promise);
        let job = j8framework::AsyncJob { resolve: a.global(resolve.into()), reject: a.global(reject.into()), future: Box::pin(delayed) };
        assert!(a.spawn(job).is_ok(), "the event loop takes host work");
        let p = a.global(promise.into());
        let v = a.local(&p);
        a.realm.global.borrow_mut(a.mc).define("p", j8framework::Property::data(v));
    });
    lp.eval(&Source { text: "globalThis.log = []; p.then(v => log.push('settled ' + v + ' after ' + log.join()));", file: "a.js" }, ScriptKind::Classic).expect("runs");
    let started = Instant::now();
    let t = lp.submit(&Source { text: &format!("{}; log.push('long done'); n", long(0)), file: "long.js" }, ScriptKind::Classic);
    let g = lp.wait(t).expect("runs");
    let finished = Instant::now();
    assert_eq!(text(&mut lp, &g), long_value(0));
    // Polled while the script was still running: the queue is checked between its slices
    let ready = *ready_at.get().expect("the future was polled ready");
    assert!(ready < finished && finished - ready > (finished - started) / 4, "polled at {:?} of a {:?} script", ready - started, finished - started);
    // ...but its JS waited for the script (run-to-completion)
    lp.run().expect("settles");
    let log = lp.eval(&Source { text: "log.join(' | ')", file: "b.js" }, ScriptKind::Classic).expect("runs");
    assert_eq!(text(&mut lp, &log), "long done | settled 7 after long done");
}
