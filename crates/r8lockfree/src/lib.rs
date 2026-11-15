//! r8lockfree：r8 的无锁基础件（手写，`no_std` + `alloc`），r8gc 与 r8taskqueue 共用。
//!
//! - [`Fifo`]：多生产者多消费者先进先出队列（位置单调递增、分段槽表）
//! - [`SegVec`]：只追加的分段向量，下标稳定、可并发读；整理与取出要独占
//! - [`PushList`]：只压入的 Treiber 栈，整表取走或封口（封口后压入被退回）
//! - [`Deque`]：Chase–Lev 工作窃取双端队列，主人凭据 [`Owner`] 压 / 弹，别人偷
//!
//! 共同的做法：并发期间只增加内存、不释放（段、旧缓冲），释放推迟到独占（`&mut` / 析构）时，
//! 因此不需要危险指针或纪元回收，也没有 ABA。
#![no_std]
extern crate alloc;

mod deque;
mod fifo;
mod pushlist;
mod segments;
mod segvec;

pub use deque::{Deque, Owner};
pub use fifo::Fifo;
pub use pushlist::{Drain, PushList};
pub use segvec::SegVec;
