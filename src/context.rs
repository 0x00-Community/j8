//! 回收器：mutator 一侧（分配、写屏障，单线程）与回收的各阶段（标记、次回收、清扫、压缩，在线程池上并行）。
use alloc::vec::Vec;
use core::cell::{Cell, RefCell};
use core::mem::take;
use core::ptr::NonNull;

use crate::collect::{Mover, Visit};
use crate::header::{Claim, Flags, Ptr, SizeClass, Typed, VTable};
use crate::heap::{Lab, NURSERY_LIMIT, Nursery, OldSpace};
use crate::parallel::{Pool, Spin, Work, Worker, split};
use crate::{Collect, Metrics, Relocate, Trace};

/// 老年代至少长到这么大才开始第一轮主回收
const MIN_HEAP: usize = 8 << 20;
/// 主回收后，下一轮在老年代长到活对象的几倍时开始
const GROWTH: usize = 2;
/// 增量标记每步至少做这么多字节
const MARK_STEP: usize = 256 << 10;
/// 增量标记每步做的量是这段时间老年代长出来的几倍
const MARK_RATIO: usize = 2;

/// 主回收进行到哪了。
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Phase {
    /// 没在主回收
    Sleep,
    /// 增量标记中
    Mark,
}

/// 纪元 `epoch` 下「标记过」的标记位
fn marked_bit(epoch: bool) -> u32 {
    if epoch { Flags::MARK } else { 0 }
}

/// 标记的工人：认领白对象压栈，出栈时遍历变黑。
struct Marker {
    epoch: bool,
    stack: Vec<Ptr>,
}

impl Marker {
    fn new(epoch: bool) -> Self {
        Self { epoch, stack: Vec::new() }
    }
}

impl Visit for Marker {
    /// 白的认领成灰压栈（不用遍历的直接变黑）；别的线程先认领了就算了
    #[inline]
    fn visit(&mut self, p: Ptr) {
        let needs = p.vtable().needs_trace;
        let epoch = self.epoch;
        let claimed = p.header().update(|f| {
            if f & Flags::GRAY != 0 || (f & Flags::MARK != 0) == epoch {
                None
            } else if needs {
                Some(f | Flags::GRAY)
            } else {
                Some(f ^ Flags::MARK)
            }
        });
        if claimed && needs {
            self.stack.push(p);
        }
    }
}

impl Worker for Marker {
    fn stack(&mut self) -> &mut Vec<Ptr> {
        &mut self.stack
    }

    fn process(&mut self, p: Ptr) -> usize {
        let marked = marked_bit(self.epoch);
        p.header().update(|f| Some(f & !(Flags::GRAY | Flags::MARK) | marked));
        // SAFETY: 灰对象归认领它的线程，mutator 停着
        unsafe { p.trace(&mut Trace::new(self)) };
        p.vtable().size
    }
}

/// 次回收的工人：把新生代对象搬进老年代，改搬过去的对象的字段。
struct Promoter<'a> {
    lab: Lab<'a>,
    epoch: bool,
    /// 主回收在标记：搬过去的要进灰队列
    marking: bool,
    /// 搬过去、字段还没改的
    stack: Vec<Ptr>,
    gray: Vec<Ptr>,
    promoted: usize,
}

impl<'a> Promoter<'a> {
    fn new(old: &'a Spin<OldSpace>, epoch: bool, marking: bool) -> Self {
        Self { lab: Lab::new(old), epoch, marking, stack: Vec::new(), gray: Vec::new(), promoted: 0 }
    }
}

impl Mover for Promoter<'_> {
    /// 新生代对象搬进老年代（别的线程搬过了就用它搬的）
    fn forward(&mut self, p: Ptr) -> Ptr {
        let h = p.header();
        match h.claim() {
            Claim::Old => p,
            Claim::Forwarded(q) => q,
            Claim::Mine => {
                let vt = p.vtable();
                let q = self.lab.slot(vt.class);
                // 标记中搬过去的这一轮都留着：要遍历的进灰队列，不用遍历的直接黑
                let gray = self.marking && vt.needs_trace;
                let mark = if gray { Flags::GRAY | (marked_bit(self.epoch) ^ Flags::MARK) } else { marked_bit(self.epoch) };
                // SAFETY: 归我的空槽；p 由 BUSY 位独占
                unsafe { p.copy_to(vt, q, Flags::OLD | mark) };
                h.forward(q);
                if gray {
                    self.gray.push(q);
                }
                if vt.needs_trace {
                    self.stack.push(q);
                }
                self.promoted += 1;
                q
            }
        }
    }
}

impl Worker for Promoter<'_> {
    fn stack(&mut self) -> &mut Vec<Ptr> {
        &mut self.stack
    }

    fn process(&mut self, p: Ptr) -> usize {
        // SAFETY: 刚搬过去的或记忆集里的老对象，各归一个线程
        unsafe { p.relocate(&mut Relocate::new(self)) };
        p.vtable().size
    }
}

/// 压缩时改字段：指向已搬走对象的换成新地址。
struct Compactor;

impl Mover for Compactor {
    fn forward(&mut self, p: Ptr) -> Ptr {
        p.header().forwarded().unwrap_or(p)
    }
}

pub(crate) struct Context {
    phase: Cell<Phase>,
    /// 标记位等于它才算标记过
    epoch: Cell<bool>,
    /// 新生代当前块的碰撞区
    cursor: Cell<usize>,
    end: Cell<usize>,
    nursery: RefCell<Nursery>,
    old: Spin<OldSpace>,
    gray: RefCell<Vec<Ptr>>,
    /// 可能指向新生代的老对象
    remembered: RefCell<Vec<Ptr>>,
    /// 老年代长到这么大开始主回收
    threshold: Cell<usize>,
    /// 上一步标记时老年代的大小
    marked_at: Cell<usize>,
    pub(crate) metrics: Metrics,
}

impl Context {
    pub(crate) fn new() -> Self {
        Self {
            phase: Cell::new(Phase::Sleep),
            epoch: Cell::new(false),
            cursor: Cell::new(0),
            end: Cell::new(0),
            nursery: RefCell::default(),
            old: Spin::default(),
            gray: RefCell::default(),
            remembered: RefCell::default(),
            threshold: Cell::new(MIN_HEAP),
            marked_at: Cell::new(0),
            metrics: Metrics::default(),
        }
    }

    pub(crate) fn phase(&self) -> Phase {
        self.phase.get()
    }

    pub(crate) fn young_bytes(&self) -> usize {
        self.nursery.borrow().used(self.cursor.get())
    }

    pub(crate) fn old_bytes(&self) -> usize {
        self.old.lock().bytes
    }

    #[inline]
    fn is_marked(&self, f: u32) -> bool {
        (f & Flags::MARK != 0) == self.epoch.get()
    }

    /// 新对象的标记位：标记中算没标记（可达的会被标到）；不在标记中算标记过（下轮翻纪元后自然没标记）
    #[inline]
    fn fresh(&self) -> u32 {
        match self.phase.get() {
            Phase::Mark => marked_bit(self.epoch.get()) ^ Flags::MARK,
            Phase::Sleep => marked_bit(self.epoch.get()),
        }
    }

    // ---- 分配（mutator） ----

    #[inline]
    pub(crate) fn alloc<'gc, T: Collect<'gc>>(&self, value: T) -> Ptr {
        let vt = &<T as Typed<'gc>>::VTABLE;
        let p = if vt.class == SizeClass::LARGE { self.alloc_large(vt) } else { self.bump(vt) };
        // SAFETY: 刚分到的槽，按 T 的大小与对齐
        unsafe { p.cast::<T>().write(value) };
        p
    }

    #[inline]
    fn bump(&self, vt: &VTable) -> Ptr {
        let mut at = self.cursor.get();
        if self.end.get() - at < vt.slot {
            at = self.refill();
        }
        self.cursor.set(at + vt.slot);
        let p = Ptr::at_slot(NonNull::new(at as *mut u8).expect("nursery slot"));
        // SAFETY: 碰撞区里新划出的槽
        unsafe { p.init(vt, self.fresh()) };
        if vt.drop.is_some() {
            self.nursery.borrow_mut().drops.push(p);
        }
        p
    }

    #[cold]
    fn refill(&self) -> usize {
        let (start, end) = self.nursery.borrow_mut().refill();
        self.end.set(end);
        start
    }

    /// 大对象直接进老年代；字段可能指向新生代，先进记忆集
    #[cold]
    fn alloc_large(&self, vt: &VTable) -> Ptr {
        let p = self.old.lock().alloc_large(vt);
        // SAFETY: 大对象的头在值前面，属于这块分配
        unsafe { p.init(vt, Flags::OLD | Flags::LARGE | Flags::REMEMBERED | self.fresh()) };
        self.remembered.borrow_mut().push(p);
        p
    }

    // ---- 写屏障（mutator） ----

    /// 对象 `p` 要收进新指针之前调：老对象进记忆集；标记中的黑对象变回灰
    #[inline]
    pub(crate) fn barrier(&self, p: Ptr) {
        let f = p.header().flags();
        if f & (Flags::OLD | Flags::REMEMBERED) == Flags::OLD {
            self.remember(p);
        }
        if self.phase.get() == Phase::Mark && f & Flags::GRAY == 0 && self.is_marked(f) {
            self.gray_again(p);
        }
    }

    #[cold]
    fn remember(&self, p: Ptr) {
        p.header().or_flags(Flags::REMEMBERED);
        self.remembered.borrow_mut().push(p);
    }

    #[cold]
    fn gray_again(&self, p: Ptr) {
        p.header().or_flags(Flags::GRAY);
        self.gray.borrow_mut().push(p);
    }

    // ---- 标记 ----

    /// 并行标记：从 `initial` 出发；给了 `budget` 就做到那么多字节为止，返回没做完的
    fn mark(&self, pool: &dyn Pool, initial: Vec<Ptr>, budget: Option<usize>) -> Vec<Ptr> {
        if initial.is_empty() {
            return initial;
        }
        let threads = pool.threads();
        let mut workers: Vec<Marker> = (0..threads).map(|_| Marker::new(self.epoch.get())).collect();
        Work::run(pool, initial, &mut workers, budget.map(|b| b.div_ceil(threads)))
    }

    fn start_major<'gc, R: Collect<'gc> + ?Sized>(&mut self, root: &R) {
        self.epoch.set(!self.epoch.get());
        self.phase.set(Phase::Mark);
        self.marked_at.set(self.old_bytes());
        let mut m = Marker::new(self.epoch.get());
        Trace::new(&mut m).trace(root);
        self.gray.get_mut().append(&mut m.stack);
    }

    /// 标记一步；灰的做完了返回 `true`
    fn step(&mut self, pool: &dyn Pool) -> bool {
        let old = self.old_bytes();
        let budget = MARK_STEP.max(old.saturating_sub(self.marked_at.get()) * MARK_RATIO);
        self.marked_at.set(old);
        let initial = take(self.gray.get_mut());
        let left = self.mark(pool, initial, Some(budget));
        *self.gray.get_mut() = left;
        self.gray.get_mut().is_empty()
    }

    // ---- 次回收 ----

    /// 次回收：从根和记忆集出发，把活着的新对象并行搬进老年代，其余析构，新生代清空
    pub(crate) fn minor<'gc, R: Collect<'gc> + ?Sized>(&mut self, pool: &dyn Pool, root: &mut R) {
        let (epoch, marking) = (self.epoch.get(), self.phase.get() == Phase::Mark);
        let remembered = take(self.remembered.get_mut());
        let (gray, promoted) = {
            let mut workers: Vec<Promoter<'_>> = (0..pool.threads()).map(|_| Promoter::new(&self.old, epoch, marking)).collect();
            // 根只有一个，在调用线程上改；它直接引用的新对象搬过去后进初始工作
            Relocate::new(&mut workers[0]).relocate(root);
            let mut initial = take(&mut workers[0].stack);
            for p in remembered {
                p.header().and_flags(!Flags::REMEMBERED);
                if p.vtable().needs_trace {
                    initial.push(p);
                }
            }
            let left = Work::run(pool, initial, &mut workers, None);
            debug_assert!(left.is_empty());
            let gray: Vec<Ptr> = workers.iter_mut().flat_map(|w| take(&mut w.gray)).collect();
            (gray, workers.iter().map(|w| w.promoted).sum::<usize>())
        };
        for p in take(&mut self.nursery.get_mut().drops) {
            if p.header().forwarded().is_none() {
                // SAFETY: 没被搬走 ⇒ 不可达；只析构这一次
                unsafe { p.drop_value() };
            }
        }
        let g = self.gray.get_mut();
        if marking {
            // 灰队列里的新对象：活的已在老年代另排了队，死的不要了
            g.retain(|p| p.header().flags() & Flags::OLD != 0);
            g.extend(gray);
        }
        self.nursery.get_mut().reset();
        self.cursor.set(0);
        self.end.set(0);
        self.metrics.minor.set(self.metrics.minor.get() + 1);
        self.metrics.promoted.set(self.metrics.promoted.get() + promoted);
        self.verify("after minor");
    }

    // ---- 主回收收尾 ----

    /// 标记做完（并行、原子）、清扫（并行）、值得就压缩（并行）
    fn finish<'gc, R: Collect<'gc> + ?Sized>(&mut self, pool: &dyn Pool, root: &mut R) {
        self.minor(pool, root);
        let mut m = Marker::new(self.epoch.get());
        Trace::new(&mut m).trace(&*root);
        let mut initial = take(self.gray.get_mut());
        initial.append(&mut m.stack);
        let left = self.mark(pool, initial, None);
        debug_assert!(left.is_empty());
        let live = self.old.get_mut().sweep(pool, self.epoch.get());
        self.verify("after sweep");
        self.phase.set(Phase::Sleep);
        self.compact(pool, root);
        self.verify("after compact");
        self.threshold.set(MIN_HEAP.max(live * GROWTH));
        self.metrics.major.set(self.metrics.major.get() + 1);
    }

    /// 搬空稀疏的页（并行复制），再把所有指向它们的字段改过来（并行）
    fn compact<'gc, R: Collect<'gc> + ?Sized>(&mut self, pool: &dyn Pool, root: &mut R) {
        let mut moving = self.old.get_mut().select_evacuation();
        if moving.is_empty() {
            return;
        }
        let flags = Flags::OLD | marked_bit(self.epoch.get());
        let moved = moving.iter().map(|&(_, class)| SizeClass::SLOTS[class]).sum();
        let old = &self.old;
        split(pool, &mut moving, &|chunk: &mut [(Ptr, usize)]| {
            let mut lab = Lab::new(old);
            for &(p, class) in &*chunk {
                let vt = p.vtable();
                let q = lab.slot(class);
                // SAFETY: 归这个任务的空槽；p 在要搬空的页里，只出现一次
                unsafe { p.copy_to(vt, q, flags) };
                p.header().forward(q);
            }
        });
        Relocate::new(&mut Compactor).relocate(root);
        let mut live = self.old.get_mut().live();
        split(pool, &mut live, &|chunk: &mut [Ptr]| {
            for &p in chunk.iter().filter(|p| p.vtable().needs_trace) {
                // SAFETY: 活着的老对象，各归一个任务
                unsafe { p.relocate(&mut Relocate::new(&mut Compactor)) };
            }
        });
        self.old.get_mut().release_evacuated(moved);
        self.metrics.compactions.set(self.metrics.compactions.get() + 1);
    }

    // ---- 节奏 ----

    /// 按欠账做一段：新生代满了做次回收；老年代过了阈值开始 / 推进主回收
    pub(crate) fn collect_debt<'gc, R: Collect<'gc> + ?Sized>(&mut self, pool: &dyn Pool, root: &mut R) {
        if self.young_bytes() >= NURSERY_LIMIT {
            self.minor(pool, root);
        }
        match self.phase.get() {
            Phase::Sleep if self.old_bytes() >= self.threshold.get() => self.start_major(root),
            Phase::Mark if self.step(pool) => self.finish(pool, root),
            Phase::Sleep | Phase::Mark => {}
        }
    }

    /// 完整回收一轮（含次回收与压缩）
    pub(crate) fn collect_all<'gc, R: Collect<'gc> + ?Sized>(&mut self, pool: &dyn Pool, root: &mut R) {
        if self.phase.get() == Phase::Sleep {
            self.start_major(root);
        }
        self.finish(pool, root);
    }

    // ---- 调试校验（feature `verify`） ----

    /// 老年代每个对象的每条边：目标得是活着、没搬走、不是空槽的老对象（新生代须为空）
    #[allow(clippy::unused_self)]
    fn verify(&mut self, stage: &'static str) {
        #[cfg(feature = "verify")]
        {
            /// 校验一条边
            struct Verifier<'a> {
                old: &'a OldSpace,
                stage: &'static str,
                owner: Ptr,
            }
            impl Visit for Verifier<'_> {
                fn visit(&mut self, p: Ptr) {
                    let place = self.old.place(p);
                    assert!(place != "unknown", "{}: edge to unknown memory {p:?} from {:?}", self.stage, self.owner);
                    let f = p.header().flags();
                    assert!(f & (Flags::FORWARDED | Flags::FREE) == 0 && f & Flags::OLD != 0, "{}: edge to {p:?} ({place}) flags {f:#x}", self.stage);
                }
            }
            let old = self.old.get_mut();
            for p in old.live().into_iter().filter(|p| p.vtable().needs_trace) {
                let mut v = Verifier { old, stage, owner: p };
                // SAFETY: 活着的老对象
                unsafe { p.trace(&mut Trace::new(&mut v)) };
            }
        }
        let _ = stage;
    }
}

impl Drop for Context {
    fn drop(&mut self) {
        for p in take(&mut self.nursery.get_mut().drops) {
            if p.header().forwarded().is_none() {
                // SAFETY: 整个堆在拆，新对象各析构一次（老年代的由 OldSpace 析构）
                unsafe { p.drop_value() };
            }
        }
    }
}
