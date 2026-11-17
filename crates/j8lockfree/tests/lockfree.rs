//! 多线程压力：每个元素恰好经手一次，次序约束成立。Miri 下自动缩小规模。
use std::sync::Barrier;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::thread;

use std::sync::Arc;

use j8lockfree::{Chain, Deque, Fifo, Prefix, PushList, SegVec};

/// 每个线程的元素数
#[cfg(not(miri))]
const N: usize = 20_000;
#[cfg(miri)]
const N: usize = 40;
/// 线程数
const THREADS: usize = 4;

/// 元素编码：高位是生产者，低位是序号
fn tag(producer: usize, i: usize) -> usize {
    producer * N + i
}

/// 每个元素恰好出现一次
fn exactly_once(seen: &[AtomicUsize]) {
    for (v, c) in seen.iter().enumerate() {
        assert_eq!(c.load(Ordering::Relaxed), 1, "元素 {v} 出现了 {} 次", c.load(Ordering::Relaxed));
    }
}

fn counters(n: usize) -> Vec<AtomicUsize> {
    (0..n).map(|_| AtomicUsize::new(0)).collect()
}

/// 多生产者多消费者：每个元素恰好取到一次；同一个消费者看到的同一生产者的元素按生产次序递增。
#[test]
fn fifo_mpmc() {
    let q = Fifo::default();
    let seen = counters(THREADS * N);
    let taken = AtomicUsize::new(0);
    thread::scope(|s| {
        for p in 0..THREADS {
            let q = &q;
            s.spawn(move || (0..N).for_each(|i| q.push(tag(p, i))));
        }
        for _ in 0..THREADS {
            s.spawn(|| {
                let mut last = [None; THREADS];
                while taken.load(Ordering::Acquire) < THREADS * N {
                    if let Some(v) = q.pop() {
                        taken.fetch_add(1, Ordering::AcqRel);
                        seen[v].fetch_add(1, Ordering::Relaxed);
                        let (p, i) = (v / N, v % N);
                        assert!(last[p].is_none_or(|l| l < i), "生产者 {p} 的元素乱序");
                        last[p] = Some(i);
                    }
                }
            });
        }
    });
    exactly_once(&seen);
    assert!(q.is_empty());
}

/// 整理之后剩下的次序不变、能接着用。
#[test]
fn fifo_compact_keeps_order() {
    let mut q = Fifo::default();
    (0..N).for_each(|i| q.push(i));
    let half = N / 2;
    (0..half).for_each(|i| assert_eq!(q.pop(), Some(i)));
    q.compact();
    q.push(N);
    assert_eq!(core::iter::from_fn(|| q.pop()).collect::<Vec<_>>(), (half..=N).collect::<Vec<_>>());
}

/// 并发追加：下标各不相同、取出来一个不少；读到的值就是写进去的值。
#[test]
fn segvec_concurrent_push() {
    let mut v = SegVec::default();
    thread::scope(|s| {
        for p in 0..THREADS {
            let v = &v;
            s.spawn(move || {
                for i in 0..N {
                    let at = v.push(tag(p, i));
                    assert_eq!(v.get(at), Some(&tag(p, i)));
                }
            });
        }
    });
    assert_eq!(v.len(), THREADS * N);
    let seen = counters(THREADS * N);
    v.iter_mut().for_each(|x| *x += 0);
    for x in v.take() {
        seen[x].fetch_add(1, Ordering::Relaxed);
    }
    exactly_once(&seen);
    assert!(v.is_empty());
}

/// 并发压入与取走：每个元素恰好取到一次。
#[test]
fn pushlist_push_and_take() {
    let list = PushList::default();
    let seen = counters(THREADS * N);
    let done = AtomicUsize::new(0);
    thread::scope(|s| {
        for p in 0..THREADS {
            let (list, done) = (&list, &done);
            s.spawn(move || {
                (0..N).for_each(|i| list.push(tag(p, i)).unwrap());
                done.fetch_add(1, Ordering::Release);
            });
        }
        s.spawn(|| {
            loop {
                let finished = done.load(Ordering::Acquire) == THREADS;
                list.take().for_each(|v| {
                    seen[v].fetch_add(1, Ordering::Relaxed);
                });
                if finished {
                    break;
                }
            }
        });
    });
    exactly_once(&seen);
}

/// 封口：之前压进去的都被封口取走，之后的都被退回；两边加起来不重不漏。
#[test]
fn pushlist_seal() {
    let list = PushList::default();
    let seen = counters(THREADS * N);
    let start = Barrier::new(THREADS + 1);
    let sealed = AtomicBool::new(false);
    thread::scope(|s| {
        for p in 0..THREADS {
            let (list, seen, start) = (&list, &seen, &start);
            s.spawn(move || {
                start.wait();
                for i in 0..N {
                    if let Err(v) = list.push(tag(p, i)) {
                        seen[v].fetch_add(1, Ordering::Relaxed);
                    }
                }
            });
        }
        start.wait();
        list.seal().for_each(|v| {
            seen[v].fetch_add(1, Ordering::Relaxed);
        });
        sealed.store(true, Ordering::Release);
    });
    assert!(sealed.load(Ordering::Acquire) && list.is_sealed());
    exactly_once(&seen);
}

/// 主人压 / 弹、别人偷（含扩容）：每个元素恰好拿到一次。
#[test]
fn deque_owner_and_thieves() {
    let d: Deque<usize> = Deque::default();
    let seen = counters(THREADS * N);
    let finished = AtomicBool::new(false);
    thread::scope(|s| {
        for _ in 0..THREADS - 1 {
            s.spawn(|| {
                while !finished.load(Ordering::Acquire) || !d.is_empty() {
                    if let Some(v) = d.steal() {
                        seen[v].fetch_add(1, Ordering::Relaxed);
                    }
                }
            });
        }
        let owner = d.owner().unwrap();
        assert!(d.owner().is_none(), "主人只能有一个");
        for i in 0..THREADS * N {
            owner.push(i);
            // 时不时自己弹一个，制造与偷的人抢最后一个的情形
            if i % 3 == 0
                && let Some(v) = owner.pop()
            {
                seen[v].fetch_add(1, Ordering::Relaxed);
            }
        }
        while let Some(v) = owner.pop() {
            seen[v].fetch_add(1, Ordering::Relaxed);
        }
        finished.store(true, Ordering::Release);
    });
    exactly_once(&seen);
    assert!(d.owner().is_some(), "凭据析构后可以再领");
}

/// 库存：并发领取不重不漏；还回来的在整理后能再领到。
#[test]
fn stock_claim_give_settle() {
    let mut stock = j8lockfree::Stock::default();
    stock.extend(0..THREADS * N);
    let seen = counters(THREADS * N);
    let batch = 7;
    thread::scope(|s| {
        for _ in 0..THREADS {
            let (stock, seen) = (&stock, &seen);
            s.spawn(move || {
                let mut out = Vec::new();
                while stock.claim(batch, &mut out) > 0 {}
                // 一半还回去
                let back = out.split_off(out.len() / 2);
                for &v in &out {
                    seen[v].fetch_add(1, Ordering::Relaxed);
                }
                stock.give(back);
            });
        }
    });
    assert!(stock.is_empty());
    stock.settle();
    let mut out = Vec::new();
    while stock.claim(batch, &mut out) > 0 {}
    for &v in &out {
        seen[v].fetch_add(1, Ordering::Relaxed);
    }
    exactly_once(&seen);
}

/// 并发压入：每个元素恰好在表里一次；push 给的引用就是自己压的值
#[test]
fn chain_concurrent_push() {
    let c = Chain::default();
    thread::scope(|s| {
        for p in 0..THREADS {
            let c = &c;
            s.spawn(move || {
                for i in 0..N {
                    assert_eq!(*c.push(tag(p, i)), tag(p, i));
                }
            });
        }
    });
    let seen = counters(THREADS * N);
    for &v in c.iter() {
        seen[v].fetch_add(1, Ordering::Relaxed);
    }
    exactly_once(&seen);
}

/// 好几个持有者同时站在同一个末尾追加：恰好一个原地写（拿到同一块），其余各自复制；每个人的前缀都对
#[test]
fn prefix_one_extends_in_place() {
    let base = Prefix::new((0..3).collect::<Vec<usize>>());
    let start = Barrier::new(THREADS);
    let grown: Vec<Arc<Prefix<usize>>> = thread::scope(|s| {
        let handles: Vec<_> = (0..THREADS)
            .map(|p| {
                let (base, start) = (&base, &start);
                s.spawn(move || {
                    start.wait();
                    base.extend(3, 100 + p)
                })
            })
            .collect();
        handles.into_iter().map(|h| h.join().expect("thread")).collect()
    });
    assert_eq!(grown.iter().filter(|g| Arc::ptr_eq(g, &base)).count(), 1, "exactly one extends in place");
    for (p, g) in grown.iter().enumerate() {
        assert_eq!(g.prefix(4), [0, 1, 2, 100 + p], "holder {p} sees its own item");
    }
}

/// 一条链一直接着追加：前缀始终对，缓冲区只在写满时换（换的次数是对数级）
#[test]
fn prefix_chain_grows() {
    let mut cur = Prefix::new(Vec::new());
    let mut blocks = 1;
    for i in 0..N {
        let next = cur.extend(i, i);
        if !Arc::ptr_eq(&next, &cur) {
            blocks += 1;
        }
        cur = next;
        assert_eq!(cur.prefix(i + 1)[i], i);
    }
    assert_eq!(cur.prefix(N), (0..N).collect::<Vec<_>>());
    assert!(blocks <= usize::BITS as usize, "{blocks} buffers for {N} items");
}
