//! 从别的线程中止 JS（V8 的 `TerminateExecution`）。[`IsolateHandle::terminate`] 置请求，执行层在检查点（函数入口、回跳；
//! JIT 机器码在回跳）看到后抛 [`Error::Terminated`](crate::Error::Terminated)：不是 JS 异常，`catch` / `finally` 都接不住，
//! 一路传到最外层。传出最外层（[`Isolate::enter`](crate::Isolate::enter) 返回）时自动复位，isolate 接着能用；没被看到的
//! 请求留到下次进 JS。
use alloc::sync::Arc;
use core::sync::atomic::{AtomicU8, Ordering};

/// 中止状态。JIT 机器码按字节读它：非零就退回解释器。
#[derive(Debug, Default)]
#[repr(transparent)]
pub struct Termination(AtomicU8);

/// 中止状态的取值
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[repr(u8)]
enum State {
    /// 没有请求
    Idle = 0,
    /// 有请求，还没有执行层看到
    Requested = 1,
    /// 执行层已看到，正在往外退
    Unwinding = 2,
}

impl Termination {
    /// 请求中止
    pub fn request(&self) {
        let _ = self.0.compare_exchange(State::Idle as u8, State::Requested as u8, Ordering::SeqCst, Ordering::SeqCst);
    }

    /// 撤销请求（也停下正在往外退的中止：之后的检查点不再抛）
    pub fn cancel(&self) {
        self.0.store(State::Idle as u8, Ordering::SeqCst);
    }

    /// 是否有请求或正在往外退（V8 的 `IsExecutionTerminating`）
    pub fn is_set(&self) -> bool {
        self.0.load(Ordering::Relaxed) != State::Idle as u8
    }

    /// 执行层的检查点：有请求就转成往外退，返回 `true`（此后同一次进入里的检查点都返回 `true`）
    pub fn observe(&self) -> bool {
        if !self.is_set() {
            return false;
        }
        let _ = self.0.compare_exchange(State::Requested as u8, State::Unwinding as u8, Ordering::SeqCst, Ordering::SeqCst);
        true
    }

    /// 传出最外层：往外退的复位成没有请求（只看到过请求、还没退的不动）
    pub fn finish(&self) {
        let _ = self.0.compare_exchange(State::Unwinding as u8, State::Idle as u8, Ordering::SeqCst, Ordering::SeqCst);
    }

    /// 给 JIT 机器码读的字节地址
    pub fn as_ptr(&self) -> *const u8 {
        self.0.as_ptr().cast_const()
    }
}

/// 别的线程拿着用的 isolate 句柄（V8 的 `IsolateHandle`）：可以随意 clone、跨线程传。
#[derive(Clone, Debug)]
pub struct IsolateHandle(pub Arc<Termination>);

impl IsolateHandle {
    /// 中止正在跑（或下一次要跑）的 JS
    pub fn terminate(&self) {
        self.0.request();
    }

    /// 撤销中止
    pub fn cancel_terminate(&self) {
        self.0.cancel();
    }

    /// 是否正在中止
    pub fn is_terminating(&self) -> bool {
        self.0.is_set()
    }
}
