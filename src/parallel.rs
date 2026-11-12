//! 并行：线程池接口（moremath 的任何 `Executor` 都是）、自旋锁、工作窃取的驱动。
use alloc::boxed::Box;
use alloc::vec::Vec;
use core::cell::UnsafeCell;
use core::hint::spin_loop;
use core::ops::{Deref, DerefMut};
use core::sync::atomic::{AtomicBool, AtomicUsize, Ordering};

use moremath::Executor;

use crate::header::Ptr;

/// 交给线程池的一个任务：借用 `'t` 的数据，跑完才返回。
pub type Job<'t> = Box<dyn FnOnce() + Send + 't>;

/// 回收用的线程池。moremath 的 `ThreadSingle` / `ThreadMulti` 都实现了它；
/// 嵌入方也可以拿自己的池实现（几个堆可以共用一个池）。
pub trait Pool {
    /// 同时能跑几个任务
    fn threads(&self) -> usize;

    /// 跑完 `jobs` 才返回（任务可以借用调用方的数据）
    fn run(&self, jobs: Vec<Job<'_>>);
}

impl<X: Executor> Pool for X {
    fn threads(&self) -> usize {
        Executor::threads(self)
    }

    fn run(&self, jobs: Vec<Job<'_>>) {
        self.execute(jobs);
    }
}

/// 等别的线程：先自旋，久了让出 CPU（线程比核多、持锁的人被换下去时不陪着空转）。
#[derive(Default)]
pub(crate) struct Backoff(u32);

impl Backoff {
    /// 自旋这么多轮之后改为让出 CPU
    const SPINS: u32 = 64;

    pub(crate) fn wait(&mut self) {
        if self.0 < Self::SPINS {
            self.0 += 1;
            spin_loop();
        } else {
            #[cfg(feature = "std")]
            std::thread::yield_now();
            #[cfg(not(feature = "std"))]
            spin_loop();
        }
    }
}

/// 很短的临界区用的自旋锁（`no_std`）。
pub(crate) struct Spin<T> {
    locked: AtomicBool,
    value: UnsafeCell<T>,
}

// SAFETY: 同一时刻只有拿到锁的线程碰 value
unsafe impl<T: Send> Sync for Spin<T> {}
unsafe impl<T: Send> Send for Spin<T> {}

impl<T: Default> Default for Spin<T> {
    fn default() -> Self {
        Self::new(T::default())
    }
}

impl<T> Spin<T> {
    pub(crate) fn new(value: T) -> Self {
        Self { locked: AtomicBool::new(false), value: UnsafeCell::new(value) }
    }

    pub(crate) fn lock(&self) -> SpinGuard<'_, T> {
        let mut backoff = Backoff::default();
        while self.locked.compare_exchange_weak(false, true, Ordering::Acquire, Ordering::Relaxed).is_err() {
            backoff.wait();
        }
        SpinGuard(self)
    }

    pub(crate) fn get_mut(&mut self) -> &mut T {
        self.value.get_mut()
    }
}

pub(crate) struct SpinGuard<'a, T>(&'a Spin<T>);

impl<T> Deref for SpinGuard<'_, T> {
    type Target = T;

    fn deref(&self) -> &T {
        // SAFETY: 拿着锁
        unsafe { &*self.0.value.get() }
    }
}

impl<T> DerefMut for SpinGuard<'_, T> {
    fn deref_mut(&mut self) -> &mut T {
        // SAFETY: 拿着锁
        unsafe { &mut *self.0.value.get() }
    }
}

impl<T> Drop for SpinGuard<'_, T> {
    fn drop(&mut self) {
        self.0.locked.store(false, Ordering::Release);
    }
}

/// 把 `items` 切成块并行处理，结果按块的顺序返回。
pub(crate) fn split<T: Send, R: Send>(pool: &dyn Pool, items: &mut [T], f: &(dyn Fn(&mut [T]) -> R + Sync)) -> Vec<R> {
    /// 每个线程分几块（块小一点，先做完的线程接着拿，忙闲均匀）
    const CHUNKS_PER_THREAD: usize = 4;
    /// 不到这么多就在调用线程上做，不惊动线程池
    const SOLO: usize = 256;
    if items.is_empty() {
        return Vec::new();
    }
    if items.len() < SOLO || pool.threads() == 1 {
        return alloc::vec![f(items)];
    }
    let size = items.len().div_ceil(pool.threads() * CHUNKS_PER_THREAD).max(1);
    let mut out: Vec<Option<R>> = items.chunks(size).map(|_| None).collect();
    let jobs: Vec<Job<'_>> = items.chunks_mut(size).zip(out.iter_mut()).map(|(chunk, slot)| Box::new(move || *slot = Some(f(chunk))) as Job<'_>).collect();
    pool.run(jobs);
    out.into_iter().map(|r| r.expect("job ran")).collect()
}

/// 工作窃取里的一个工人：有自己的栈，一次处理一个对象。
pub(crate) trait Worker: Send {
    fn stack(&mut self) -> &mut Vec<Ptr>;

    /// 处理一个对象（可能往自己的栈里压新的）；返回做了多少字节的活
    fn process(&mut self, p: Ptr) -> usize;
}

/// 工作窃取：每个工人一个本地栈，压得多了分一半到公共栈，空了从公共栈拿；全员空闲就结束。
pub(crate) struct Work {
    shared: Spin<Vec<Ptr>>,
    /// 公共栈里有几个（不拿锁就能看）
    pending: AtomicUsize,
    /// 开工了几个（线程池忙时有的工人晚开工，甚至别人都做完了才开工）
    started: AtomicUsize,
    idle: AtomicUsize,
    workers: usize,
}

impl Work {
    /// 本地栈超过它才往外分
    const SHARE: usize = 64;
    /// 调用线程先单干这么多个对象：活少就不惊动线程池
    const SOLO: usize = 4096;
    /// 空闲的工人等这么多轮还没活就收工（不陪到最后空转）
    const RETIRE: usize = 1 << 10;

    /// 用 `pool` 让 `workers` 一起做完 `initial`（及做的过程中冒出来的）。
    /// 给了 `budget` 就每个工人做到那么多字节为止；没做完的返回。
    pub(crate) fn run<W: Worker>(pool: &dyn Pool, mut initial: Vec<Ptr>, workers: &mut [W], mut budget: Option<usize>) -> Vec<Ptr> {
        let first = &mut workers[0];
        first.stack().append(&mut initial);
        for _ in 0..Self::SOLO {
            let Some(p) = first.stack().pop() else { return Vec::new() };
            let done = first.process(p).max(1);
            if let Some(b) = &mut budget {
                *b = b.saturating_sub(done);
                if *b == 0 {
                    return core::mem::take(first.stack());
                }
            }
        }
        let initial = core::mem::take(first.stack());
        if initial.is_empty() {
            return initial;
        }
        // 剩下的全放公共栈：不预先分给可能晚开工的工人
        let pending = AtomicUsize::new(initial.len());
        let work = Self { shared: Spin::new(initial), pending, started: AtomicUsize::new(0), idle: AtomicUsize::new(0), workers: workers.len() };
        let jobs: Vec<Job<'_>> = workers.iter_mut().map(|w| Box::new(|| work.drive(w, budget)) as Job<'_>).collect();
        pool.run(jobs);
        let mut left = work.shared.into_inner();
        for w in workers.iter_mut() {
            left.append(w.stack());
        }
        left
    }

    fn drive<W: Worker>(&self, w: &mut W, mut budget: Option<usize>) {
        self.started.fetch_add(1, Ordering::SeqCst);
        loop {
            while let Some(p) = w.stack().pop() {
                let done = w.process(p).max(1);
                if let Some(b) = &mut budget {
                    *b = b.saturating_sub(done);
                    if *b == 0 {
                        // 做够了：算永久空闲，剩下的留在栈里
                        self.idle.fetch_add(1, Ordering::SeqCst);
                        return;
                    }
                }
                self.share(w.stack());
            }
            if self.steal(w.stack()) {
                continue;
            }
            self.idle.fetch_add(1, Ordering::SeqCst);
            let mut backoff = Backoff::default();
            for spins in 0.. {
                if spins == Self::RETIRE {
                    // 收工：一直算空闲，别人分出来的活由还在干的人自己拿回去
                    return;
                }
                if self.pending.load(Ordering::SeqCst) > 0 {
                    self.idle.fetch_sub(1, Ordering::SeqCst);
                    break;
                }
                // 开工了的都闲着、公共栈也空：做完了（没开工的来了也只会空手走）
                if self.idle.load(Ordering::SeqCst) == self.started.load(Ordering::SeqCst) {
                    return;
                }
                backoff.wait();
            }
        }
    }

    /// 本地压多了而公共栈不多：分一半出去
    fn share(&self, stack: &mut Vec<Ptr>) {
        if stack.len() > 2 * Self::SHARE && self.pending.load(Ordering::Relaxed) < self.workers * Self::SHARE {
            let mut shared = self.shared.lock();
            shared.extend(stack.drain(..stack.len() / 2));
            self.pending.store(shared.len(), Ordering::SeqCst);
        }
    }

    /// 从公共栈拿一批
    fn steal(&self, stack: &mut Vec<Ptr>) -> bool {
        if self.pending.load(Ordering::SeqCst) == 0 {
            return false;
        }
        let mut shared = self.shared.lock();
        let take = shared.len().min(Self::SHARE);
        let from = shared.len() - take;
        stack.extend(shared.drain(from..));
        self.pending.store(shared.len(), Ordering::SeqCst);
        take > 0
    }
}

impl<T> Spin<T> {
    pub(crate) fn into_inner(self) -> T {
        self.value.into_inner()
    }
}
