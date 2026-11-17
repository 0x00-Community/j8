//! [`Chain`]：只追加的无锁单链表。CAS 压到表头；随时可以用 `&self` 遍历、拿到的引用在表活着时一直有效；
//! 节点只在独占（析构）时释放，所以没有 ABA，也不需要回收方案。本体只有一个原子指针：空表几乎不占地方，
//! 适合「大多是空的或只有一两项」的表（隐藏类的转换表）。
use alloc::boxed::Box;
use core::fmt;
use core::ptr;
use core::sync::atomic::{AtomicPtr, Ordering};

struct Node<T> {
    value: T,
    /// 压入时的表头，发布后不再改
    next: *const Node<T>,
}

pub struct Chain<T> {
    head: AtomicPtr<Node<T>>,
}

// SAFETY: 遍历把 &T 交给任意线程（要 T: Sync），值随表跨线程转手（要 T: Send）
unsafe impl<T: Send + Sync> Sync for Chain<T> {}
// SAFETY: 表拥有其中的值
unsafe impl<T: Send> Send for Chain<T> {}

impl<T> Default for Chain<T> {
    fn default() -> Self {
        Self { head: AtomicPtr::new(ptr::null_mut()) }
    }
}

impl<T> Chain<T> {
    /// 压一项；给出它在表里的引用（表活着就有效）
    pub fn push(&self, value: T) -> &T {
        let node = Box::into_raw(Box::new(Node { value, next: ptr::null() }));
        let mut cur = self.head.load(Ordering::Acquire);
        loop {
            // SAFETY: node 还没发布，只有这里碰它
            unsafe { (*node).next = cur };
            match self.head.compare_exchange_weak(cur, node, Ordering::AcqRel, Ordering::Acquire) {
                // SAFETY: 发布了的节点只在独占析构时释放
                Ok(_) => return unsafe { &(*node).value },
                Err(now) => cur = now,
            }
        }
    }

    /// 从最新压的到最早压的
    pub fn iter(&self) -> impl Iterator<Item = &T> {
        let mut p: *const Node<T> = self.head.load(Ordering::Acquire);
        core::iter::from_fn(move || {
            // SAFETY: 非空的 p 是发布了的节点，活到表析构；它的 next 在发布前写好
            let node = unsafe { p.as_ref() }?;
            p = node.next;
            Some(&node.value)
        })
    }

    pub fn is_empty(&self) -> bool {
        self.head.load(Ordering::Acquire).is_null()
    }
}

impl<T> Drop for Chain<T> {
    fn drop(&mut self) {
        let mut p = *self.head.get_mut();
        while !p.is_null() {
            // SAFETY: 独占；每个节点来自 Box::into_raw，沿 next 各释放一次
            let node = unsafe { Box::from_raw(p) };
            p = node.next.cast_mut();
        }
    }
}

impl<T: fmt::Debug> fmt::Debug for Chain<T> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_list().entries(self.iter()).finish()
    }
}
