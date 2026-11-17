//! [`OnceBox`]：只发布一次的惰性值。并发初始化时各算各的，比较交换赢的那份留下，输的丢掉；没有等待。
use alloc::boxed::Box;
use core::fmt;
use core::ptr::null_mut;
use core::sync::atomic::{AtomicPtr, Ordering};

pub struct OnceBox<T>(AtomicPtr<T>);

// SAFETY: 发布后只给出 &T；值随容器析构
unsafe impl<T: Send + Sync> Sync for OnceBox<T> {}
// SAFETY: 拥有它的值
unsafe impl<T: Send> Send for OnceBox<T> {}

impl<T> Default for OnceBox<T> {
    fn default() -> Self {
        Self(AtomicPtr::new(null_mut()))
    }
}

impl<T> OnceBox<T> {
    pub fn get(&self) -> Option<&T> {
        // SAFETY: 非空的指针来自已发布的 Box，活到容器析构
        unsafe { self.0.load(Ordering::Acquire).as_ref() }
    }

    /// 已有就返回；没有就算一份发布（别人抢先发布了就用别人的）
    pub fn get_or_init(&self, f: impl FnOnce() -> T) -> &T {
        if let Some(v) = self.get() {
            return v;
        }
        let new = Box::into_raw(Box::new(f()));
        match self.0.compare_exchange(null_mut(), new, Ordering::AcqRel, Ordering::Acquire) {
            // SAFETY: 刚发布的
            Ok(_) => unsafe { &*new },
            Err(won) => {
                // SAFETY: 没发布出去，仍归自己；赢的那份已发布
                drop(unsafe { Box::from_raw(new) });
                unsafe { &*won }
            }
        }
    }

    pub fn get_mut(&mut self) -> Option<&mut T> {
        // SAFETY: 独占
        unsafe { self.0.get_mut().as_mut() }
    }
}

impl<T> Drop for OnceBox<T> {
    fn drop(&mut self) {
        let p = *self.0.get_mut();
        if !p.is_null() {
            // SAFETY: Box::into_raw 来的，只释放一次
            drop(unsafe { Box::from_raw(p) });
        }
    }
}

impl<T: Clone> Clone for OnceBox<T> {
    fn clone(&self) -> Self {
        let c = Self::default();
        if let Some(v) = self.get() {
            c.get_or_init(|| v.clone());
        }
        c
    }
}

impl<T: fmt::Debug> fmt::Debug for OnceBox<T> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_tuple("OnceBox").field(&self.get()).finish()
    }
}
