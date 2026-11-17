//! Event loop sets: many event loops on a few pinned threads, load balancing (placement, pulling, `rebalance`,
//! `split`, moving a paused script), one quota.
use std::num::NonZeroUsize;
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
