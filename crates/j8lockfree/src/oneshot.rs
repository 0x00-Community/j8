//! [`Oneshot`]：一次性通道，一个发送方送一个值，一个接收方作为 future 等它。没有锁也不自旋：
//! 状态字只走比较交换，接收方登记 waker 与发送方送值撞上时，谁后到谁负责收尾。
//!
//! 状态：
//! - `EMPTY`：没值，没登记 waker
//! - `REGISTERING`：接收方正在写 waker（此时 waker 格子归接收方）
//! - `WAITING`：waker 已登记，等值（waker 格子归之后的发送方）
//! - `SENT`：值已写好（值格子归接收方）
use alloc::sync::Arc;
use core::cell::UnsafeCell;
use core::future::Future;
use core::pin::Pin;
use core::sync::atomic::{AtomicU8, Ordering};
use core::task::{Context, Poll, Waker};

const EMPTY: u8 = 0;
const REGISTERING: u8 = 1;
const WAITING: u8 = 2;
const SENT: u8 = 3;

/// 两端共享的格子
pub struct Oneshot<T> {
    state: AtomicU8,
    value: UnsafeCell<Option<T>>,
    waker: UnsafeCell<Option<Waker>>,
}

// SAFETY: 两个格子各自在任一时刻只归一方（见模块注释的状态），所有权靠状态字的 Acquire / Release 交接
unsafe impl<T: Send> Sync for Oneshot<T> {}
// SAFETY: 拥有其中的值
unsafe impl<T: Send> Send for Oneshot<T> {}

impl<T> Oneshot<T> {
    /// 一对新的两端
    #[allow(clippy::new_ret_no_self)]
    pub fn new() -> (Sender<T>, Receiver<T>) {
        let shared = Arc::new(Self { state: AtomicU8::new(EMPTY), value: UnsafeCell::new(None), waker: UnsafeCell::new(None) });
        (Sender(Arc::clone(&shared)), Receiver(shared))
    }

    /// 接收方取走值（只在看到 `SENT` 之后调用）
    fn take(&self) -> T {
        // SAFETY: SENT 之后值格子归接收方；接收方只有一个，且 `&mut Receiver` 保证不并发
        unsafe { (*self.value.get()).take() }.expect("Oneshot polled after it completed")
    }
}

/// 发送端：送一次就消耗掉
pub struct Sender<T>(Arc<Oneshot<T>>);

impl<T> Sender<T> {
    /// 送出值，并叫醒已登记的接收方
    pub fn send(self, v: T) {
        let o = &*self.0;
        // SAFETY: SENT 之前值格子归发送方，发送方只有一个且只送一次
        unsafe { *o.value.get() = Some(v) };
        // EMPTY：还没人等，接收方第一次 poll 就会看到值；REGISTERING：接收方写完 waker 后的比较交换会失败，自己收尾
        if o.state.swap(SENT, Ordering::AcqRel) == WAITING {
            // SAFETY: 从 WAITING 换走之后，接收方不会再碰 waker 格子（它的比较交换都会失败）
            if let Some(w) = unsafe { (*o.waker.get()).take() } {
                w.wake();
            }
        }
    }
}

/// 接收端：`.await` 得到发送方送的值（发送方一直不送就一直等）
pub struct Receiver<T>(Arc<Oneshot<T>>);

impl<T> Future for Receiver<T> {
    type Output = T;

    fn poll(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<T> {
        let o = &*self.0;
        let seen = o.state.load(Ordering::Acquire);
        if seen == SENT {
            return Poll::Ready(o.take());
        }
        // 拿下 waker 格子
        if o.state.compare_exchange(seen, REGISTERING, Ordering::Acquire, Ordering::Acquire).is_err() {
            // 只可能是发送方抢先送到了
            return Poll::Ready(o.take());
        }
        // SAFETY: REGISTERING 期间 waker 格子归接收方
        unsafe { *o.waker.get() = Some(cx.waker().clone()) };
        match o.state.compare_exchange(REGISTERING, WAITING, Ordering::AcqRel, Ordering::Acquire) {
            Ok(_) => Poll::Pending,
            Err(_) => {
                // 写 waker 期间值送到了：发送方没叫醒谁，这里直接收尾
                // SAFETY: SENT 之后发送方不再碰 waker 格子
                drop(unsafe { (*o.waker.get()).take() });
                Poll::Ready(o.take())
            }
        }
    }
}
