//! 任务节点：依赖计数、下游表、家族（并查集），全部原子操作。
//!
//! - 依赖计数 `wait`：还没做完的依赖个数；谁把它减到 0，谁把节点推进就绪队列
//! - 下游表 `succ`：Treiber 栈，CAS 压入；做完时 `swap` 成封口标记、整表取走（之后的压入看到封口 ⇒ 依赖已做完）。
//!   只有压入是并发的，弹出只在封口时独占整表，没有 ABA
//! - 家族 `parent`：并查集的父指针，CAS 只从空设一次、之后不改（不做路径压缩，免回收竞争）；
//!   总是后建的挂到先建的下面，父指针的序号严格递减 ⇒ 不成环，根是家族里最早的任务（「爹」）
use alloc::boxed::Box;
use alloc::sync::Arc;
use core::cell::UnsafeCell;
use core::fmt;
use core::ptr;
use core::sync::atomic::{AtomicBool, AtomicPtr, AtomicUsize, Ordering};

use crate::Task;
use crate::fifo::Fifo;

/// 任务的身份：[`crate::ITaskPoll::add`] 发的句柄，给别的任务的 [`crate::ITask::depend`] 用；拿着它节点就不会释放。
#[derive(Clone)]
pub struct TaskId<'a>(pub(crate) Arc<Node<'a>>);

impl<'a> TaskId<'a> {
    /// 找爹：所在家族的根，即家族里最早塞进来的任务。依赖与派生（任务里塞的）都会把两边并成一家。
    #[must_use]
    pub fn family(&self) -> TaskId<'a> {
        let root = Node::root(Arc::as_ptr(&self.0));
        if ptr::eq(root, Arc::as_ptr(&self.0)) {
            return self.clone();
        }
        // SAFETY: root 来自 Arc::as_ptr 链（整块分配的来历），经父指针链（各持一份强引用）活着；先加计数再造 Arc
        unsafe {
            Arc::increment_strong_count(root);
            TaskId(Arc::from_raw(root))
        }
    }

    /// 取消整个家族：此后轮到这家的任务不再执行，直接当做完（放行依赖它的任务）。
    pub fn cancel(&self) {
        // SAFETY: 根经父指针链活着
        unsafe { &*Node::root(Arc::as_ptr(&self.0)) }.cancelled.store(true, Ordering::Release);
    }
}

impl PartialEq for TaskId<'_> {
    fn eq(&self, other: &Self) -> bool {
        Arc::ptr_eq(&self.0, &other.0)
    }
}

impl Eq for TaskId<'_> {}

impl fmt::Debug for TaskId<'_> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "TaskId({})", self.0.seq)
    }
}

/// 下游表的一项。
struct Link<'a> {
    node: TaskId<'a>,
    next: *mut Link<'a>,
}

impl Link<'_> {
    /// 封口标记：节点已做完，不再收下游（`Link` 对齐 ≥ 2，真指针不会是 1）
    const SEALED: *mut Self = ptr::without_provenance_mut(1);
}

pub(crate) struct Node<'a> {
    /// 塞进来的次序（家族里谁先谁后）
    seq: usize,
    /// 任务本身：执行中被取走，做完是 None
    task: UnsafeCell<Option<Task<'a>>>,
    /// 还没做完的依赖个数（登记期间多 1 作保护）
    wait: AtomicUsize,
    /// 下游表头，或 [`Link::SEALED`]
    succ: AtomicPtr<Link<'a>>,
    /// 家族里的父节点（持一份强引用），根为空
    parent: AtomicPtr<Node<'a>>,
    /// 取消标记（只在根上置）
    cancelled: AtomicBool,
}

// SAFETY: task 只由当前持有执行权的线程访问（登记者、或从就绪队列弹出它的工作线程），转手经就绪队列同步；其余字段都是原子量
unsafe impl Sync for Node<'_> {}

impl<'a> Node<'a> {
    pub(crate) fn new(task: Task<'a>, seq: usize) -> Self {
        Self {
            seq,
            task: UnsafeCell::new(Some(task)),
            wait: AtomicUsize::new(1),
            succ: AtomicPtr::new(ptr::null_mut()),
            parent: AtomicPtr::new(ptr::null_mut()),
            cancelled: AtomicBool::new(false),
        }
    }

    /// 依赖表。
    ///
    /// # Safety
    /// 调用者持有执行权（节点还没进就绪队列）
    pub(crate) unsafe fn depend(&self) -> &[TaskId<'a>] {
        // SAFETY: 调用者保证独占
        unsafe { (*self.task.get()).as_ref().map_or(&[], |t| t.depend()) }
    }

    /// 取走任务去执行。
    ///
    /// # Safety
    /// 调用者刚从就绪队列弹出这个节点
    pub(crate) unsafe fn take(&self) -> Option<Task<'a>> {
        // SAFETY: 调用者保证独占
        unsafe { (*self.task.get()).take() }
    }

    /// 放回任务（再进就绪队列之前）。
    ///
    /// # Safety
    /// 调用者持有执行权
    pub(crate) unsafe fn put(&self, task: Task<'a>) {
        // SAFETY: 调用者保证独占
        unsafe { *self.task.get() = Some(task) };
    }

    /// 多等一个依赖。
    pub(crate) fn hold(&self) {
        self.wait.fetch_add(1, Ordering::AcqRel);
    }

    /// 少等一个依赖；减到 0 返回 true（调用者把它推进就绪队列）。
    pub(crate) fn release(&self) -> bool {
        self.wait.fetch_sub(1, Ordering::AcqRel) == 1
    }

    /// 登记一个下游；已封口（做完了）退回。
    pub(crate) fn push_succ(&self, node: TaskId<'a>) -> Result<(), TaskId<'a>> {
        let link = Box::into_raw(Box::new(Link { node, next: ptr::null_mut() }));
        let mut head = self.succ.load(Ordering::Acquire);
        loop {
            if head == Link::SEALED {
                // SAFETY: link 刚造出来、没压进去
                return Err(unsafe { Box::from_raw(link) }.node);
            }
            // SAFETY: link 还没发布，只有这里在写
            unsafe { (*link).next = head };
            match self.succ.compare_exchange_weak(head, link, Ordering::AcqRel, Ordering::Acquire) {
                Ok(_) => return Ok(()),
                Err(h) => head = h,
            }
        }
    }

    /// 做完：丢掉任务、封口、放行下游（等齐的推进 `ready`）。
    ///
    /// # Safety
    /// 调用者持有执行权，且每个节点只做完一次
    pub(crate) unsafe fn complete(&self, ready: &Fifo<TaskId<'a>>) {
        // SAFETY: 调用者保证独占
        drop(unsafe { (*self.task.get()).take() });
        let mut p = self.succ.swap(Link::SEALED, Ordering::AcqRel);
        while !p.is_null() && p != Link::SEALED {
            // SAFETY: 封口后整表归这里独占，每项是 push_succ 里 Box::into_raw 的
            let link = unsafe { Box::from_raw(p) };
            p = link.next;
            if link.node.0.release() {
                ready.push(link.node);
            }
        }
    }

    /// 家族的根。`this` 与返回值都是 `Arc::as_ptr` 来历的指针（能给 Arc 加减计数）。
    pub(crate) fn root(this: *const Self) -> *const Self {
        let mut cur = this;
        loop {
            // SAFETY: cur 活着（调用者持有起点，链上每个父指针持一份强引用）
            let p = unsafe { &*cur }.parent.load(Ordering::Acquire);
            if p.is_null() {
                return cur;
            }
            cur = p;
        }
    }

    /// 并成一家：两家的根里后建的挂到先建的下面。
    pub(crate) fn union(a: &Arc<Self>, b: &Arc<Self>) {
        loop {
            let (ra, rb) = (Self::root(Arc::as_ptr(a)), Self::root(Arc::as_ptr(b)));
            if ptr::eq(ra, rb) {
                return;
            }
            // SAFETY: 两个根都经 a / b 的链活着
            let (child, parent) = if unsafe { (*ra).seq > (*rb).seq } { (ra, rb) } else { (rb, ra) };
            // SAFETY: parent 是 Arc::as_ptr 来历；先替父指针加一份强引用，CAS 失败再还回去（它经 a / b 的链活着，不会归零）
            unsafe { Arc::increment_strong_count(parent) };
            // SAFETY: child 经 a / b 的链活着
            match unsafe { &*child }.parent.compare_exchange(ptr::null_mut(), parent.cast_mut(), Ordering::AcqRel, Ordering::Acquire) {
                Ok(_) => return,
                // SAFETY: 还回上面加的那份
                Err(_) => unsafe { Arc::decrement_strong_count(parent) },
            }
        }
    }

    /// 家族被取消了（从自己到根的链上有取消标记）。
    pub(crate) fn cancelled(&self) -> bool {
        let mut cur = self;
        loop {
            if cur.cancelled.load(Ordering::Acquire) {
                return true;
            }
            let p = cur.parent.load(Ordering::Acquire);
            if p.is_null() {
                return false;
            }
            // SAFETY: 同 root
            cur = unsafe { &*p };
        }
    }
}

impl Drop for Node<'_> {
    fn drop(&mut self) {
        let parent = *self.parent.get_mut();
        if !parent.is_null() {
            // SAFETY: union 里替父指针加的那份强引用
            unsafe { Arc::decrement_strong_count(parent) };
        }
        let mut p = *self.succ.get_mut();
        while !p.is_null() && p != Link::SEALED {
            // SAFETY: 独占；没封口的下游表项都是 push_succ 里 Box::into_raw 的
            let link = unsafe { Box::from_raw(p) };
            p = link.next;
        }
    }
}
