//! 只压入的 Treiber 栈：CAS 压入；取走是 `swap` 整表（取空或封口），没有并发弹出 ⇒ 没有 ABA。
//! 封口后再压入被退回（用来表示「主人已经做完，不再收」）。
use alloc::boxed::Box;
use core::ptr;
use core::sync::atomic::{AtomicPtr, Ordering};

struct Node<T> {
    value: T,
    next: *mut Node<T>,
}

pub struct PushList<T> {
    /// 表头；空指针是空表，[`PushList::SEALED`] 是已封口
    head: AtomicPtr<Node<T>>,
}

// SAFETY: 值只经压入 / 整表取走转手，要求 T: Send；表头是原子量
unsafe impl<T: Send> Sync for PushList<T> {}
// SAFETY: 只拥有 T
unsafe impl<T: Send> Send for PushList<T> {}

impl<T> Default for PushList<T> {
    fn default() -> Self {
        Self { head: AtomicPtr::new(ptr::null_mut()) }
    }
}

impl<T> PushList<T> {
    /// 封口标记（`Node` 对齐 ≥ 2，真指针不会是 1）
    const SEALED: *mut Node<T> = ptr::without_provenance_mut(1);

    /// 压入；已封口则退回。
    pub fn push(&self, value: T) -> Result<(), T> {
        let node = Box::into_raw(Box::new(Node { value, next: ptr::null_mut() }));
        let mut head = self.head.load(Ordering::Acquire);
        loop {
            if head == Self::SEALED {
                // SAFETY: node 刚造出来、没压进去
                return Err(unsafe { Box::from_raw(node) }.value);
            }
            // SAFETY: node 还没发布，只有这里在写
            unsafe { (*node).next = head };
            match self.head.compare_exchange_weak(head, node, Ordering::AcqRel, Ordering::Acquire) {
                Ok(_) => return Ok(()),
                Err(h) => head = h,
            }
        }
    }

    /// 取走整表（后压的先出），表变空；已封口的表取到空。
    pub fn take(&self) -> Drain<T> {
        let mut head = self.head.load(Ordering::Acquire);
        loop {
            if head.is_null() || head == Self::SEALED {
                return Drain(ptr::null_mut());
            }
            match self.head.compare_exchange_weak(head, ptr::null_mut(), Ordering::AcqRel, Ordering::Acquire) {
                Ok(_) => return Drain(head),
                Err(h) => head = h,
            }
        }
    }

    /// 封口并取走整表；之后的压入都被退回。
    pub fn seal(&self) -> Drain<T> {
        let head = self.head.swap(Self::SEALED, Ordering::AcqRel);
        Drain(if head == Self::SEALED { ptr::null_mut() } else { head })
    }

    pub fn is_sealed(&self) -> bool {
        self.head.load(Ordering::Acquire) == Self::SEALED
    }

    pub fn is_empty(&self) -> bool {
        let head = self.head.load(Ordering::Acquire);
        head.is_null() || head == Self::SEALED
    }
}

impl<T> Drop for PushList<T> {
    fn drop(&mut self) {
        let head = *self.head.get_mut();
        drop(Drain(if head == Self::SEALED { ptr::null_mut() } else { head }));
    }
}

/// 取走的整表，逐个给出（后压的先出）；没取完的随它析构。
pub struct Drain<T>(*mut Node<T>);

// SAFETY: 整表归它独占
unsafe impl<T: Send> Send for Drain<T> {}

impl<T> Iterator for Drain<T> {
    type Item = T;

    fn next(&mut self) -> Option<T> {
        if self.0.is_null() {
            return None;
        }
        // SAFETY: 取走后整表归这里，每个节点是 push 里 Box::into_raw 的，只拆一次
        let node = unsafe { Box::from_raw(self.0) };
        self.0 = node.next;
        Some(node.value)
    }
}

impl<T> Drop for Drain<T> {
    fn drop(&mut self) {
        for _ in self.by_ref() {}
    }
}
