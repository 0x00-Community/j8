use alloc::sync::Arc;
use core::cell::UnsafeCell;
use core::fmt;
use core::ptr;
use core::sync::atomic::{AtomicBool, AtomicPtr, AtomicUsize, Ordering};

use j8lockfree::{Fifo, PushList};

use crate::Task;

#[derive(Clone)]
pub struct TaskId<'a>(pub(crate) Arc<Node<'a>>);

impl<'a> TaskId<'a> {
    #[must_use]
    pub fn family(&self) -> TaskId<'a> {
        let root = Node::root(Arc::as_ptr(&self.0));
        if ptr::eq(root, Arc::as_ptr(&self.0)) {
            return self.clone();
        }
        unsafe {
            Arc::increment_strong_count(root);
            TaskId(Arc::from_raw(root))
        }
    }

    pub fn cancel(&self) {
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

pub(crate) struct Node<'a> {
    seq: usize,
    task: UnsafeCell<Option<Task<'a>>>,
    wait: AtomicUsize,
    succ: PushList<TaskId<'a>>,
    parent: AtomicPtr<Node<'a>>,
    cancelled: AtomicBool,
}

unsafe impl Sync for Node<'_> {}

impl<'a> Node<'a> {
    pub(crate) fn new(task: Task<'a>, seq: usize) -> Self {
        Self {
            seq,
            task: UnsafeCell::new(Some(task)),
            wait: AtomicUsize::new(1),
            succ: PushList::default(),
            parent: AtomicPtr::new(ptr::null_mut()),
            cancelled: AtomicBool::new(false),
        }
    }

    pub(crate) unsafe fn depend(&self) -> &[TaskId<'a>] {
        unsafe { (*self.task.get()).as_ref().map_or(&[], |t| t.depend()) }
    }

    pub(crate) unsafe fn take(&self) -> Option<Task<'a>> {
        unsafe { (*self.task.get()).take() }
    }

    pub(crate) unsafe fn put(&self, task: Task<'a>) {
        unsafe { *self.task.get() = Some(task) };
    }

    pub(crate) fn hold(&self) {
        self.wait.fetch_add(1, Ordering::AcqRel);
    }

    pub(crate) fn release(&self) -> bool {
        self.wait.fetch_sub(1, Ordering::AcqRel) == 1
    }

    pub(crate) fn push_succ(&self, node: TaskId<'a>) -> Result<(), TaskId<'a>> {
        self.succ.push(node)
    }

    pub(crate) unsafe fn complete(&self, ready: &Fifo<TaskId<'a>>) {
        drop(unsafe { (*self.task.get()).take() });
        for node in self.succ.seal() {
            if node.0.release() {
                ready.push(node);
            }
        }
    }

    pub(crate) fn root(this: *const Self) -> *const Self {
        let mut cur = this;
        loop {
            let p = unsafe { &*cur }.parent.load(Ordering::Acquire);
            if p.is_null() {
                return cur;
            }
            cur = p;
        }
    }

    pub(crate) fn union(a: &Arc<Self>, b: &Arc<Self>) {
        loop {
            let (ra, rb) = (Self::root(Arc::as_ptr(a)), Self::root(Arc::as_ptr(b)));
            if ptr::eq(ra, rb) {
                return;
            }
            let (child, parent) = if unsafe { (*ra).seq > (*rb).seq } { (ra, rb) } else { (rb, ra) };
            unsafe { Arc::increment_strong_count(parent) };
            match unsafe { &*child }.parent.compare_exchange(ptr::null_mut(), parent.cast_mut(), Ordering::AcqRel, Ordering::Acquire) {
                Ok(_) => return,
                Err(_) => unsafe { Arc::decrement_strong_count(parent) },
            }
        }
    }

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
            cur = unsafe { &*p };
        }
    }
}

impl Drop for Node<'_> {
    fn drop(&mut self) {
        let parent = *self.parent.get_mut();
        if !parent.is_null() {
            unsafe { Arc::decrement_strong_count(parent) };
        }
    }
}
