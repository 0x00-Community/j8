//! j8lockfree：j8 的无锁基础件（手写，`no_std` + `alloc`），j8framework 与 j8taskqueue 共用。
//!
//! - [`Chain`]：只追加的无锁单链表，可随时遍历、引用稳定；本体一个指针，适合大多为空的小表
//! - [`Fifo`]：多生产者多消费者先进先出队列（位置单调递增、分段槽表）
//! - [`SegVec`]：只追加的分段向量，下标稳定、可并发读；整理与取出要独占
//! - [`PushList`]：只压入的 Treiber 栈，整表取走或封口（封口后压入被退回）
//! - [`Stock`]：库存，独占时整理成只读数组、并发时原子游标成批领取，还回来的下次独占时并回
//! - [`Deque`]：Chase–Lev 工作窃取双端队列，主人凭据 [`Owner`] 压 / 弹，别人偷
//! - [`OnceBox`]：只发布一次的惰性值（并发初始化各算各的，赢的留下）
//! - [`Prefix`]：前缀共享的缓冲区，各持有者看前 n 项，站在末尾的 CAS 认领原地追加，分叉或写满才复制（隐藏类共用键表）
//! - [`AtomicRefCell`]：可跨线程的 `RefCell`，借用冲突时报错而不等待
//! - [`Oneshot`]：一次性通道，发送方送一个值，接收方当 future 等（[`Sender`] / [`Receiver`]）
//!
//! 共同的做法：并发期间只增加内存、不释放（段、旧缓冲），释放推迟到独占（`&mut` / 析构）时，
//! 因此不需要危险指针或纪元回收，也没有 ABA。
#![no_std]
extern crate alloc;

mod chain;
mod deque;
mod fifo;
mod once;
mod prefix;
mod oneshot;
mod pushlist;
mod refcell;
mod segments;
mod segvec;
mod stock;

pub use chain::Chain;
pub use deque::{Deque, Owner};
pub use fifo::Fifo;
pub use once::OnceBox;
pub use prefix::Prefix;
pub use oneshot::{Oneshot, Receiver, Sender};
pub use pushlist::{Drain, PushList};
pub use refcell::{AtomicRef, AtomicRefCell, AtomicRefMut};
pub use segvec::SegVec;
pub use stock::Stock;
