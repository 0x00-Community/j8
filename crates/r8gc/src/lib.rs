//! r8gc：分代、可移动（压缩）、并行的 Rust GC，`'gc` 品牌生命周期。
//!
//! - 新生代按块碰撞分配；次回收从根与记忆集出发把活对象并行复制进老年代（写屏障维护记忆集）
//! - 老年代按尺寸档分页；主回收增量标记（纪元翻转、后向写屏障），标记 / 清扫 / 压缩都在线程池上并行
//! - 线程池是借来的 [`Pool`]：moremath 的 `ThreadMulti`（`ThreadSingle`）直接能用，几个 [`R8Heap`] 可共用一个
//! - 回收只在 [`R8Heap::mutate`] 之外发生，那时所有 `Gc` 都在根下面，所以对象能搬：
//!   [`Collect`] 除了 `trace` 还有 `relocate`，把字段改成搬后的地址
//!
//! API 沿用 gc-arena 的形状（`Gc` / `Lock` / `RefLock` / `Rootable!` / `DynamicRootSet`）。
#![no_std]
extern crate alloc;
#[cfg(feature = "std")]
extern crate std;

mod arena;
mod collect;
mod context;
mod gc;
mod header;
mod heap;
mod lock;
mod metrics;
mod parallel;
mod roots;
mod static_wrapper;

pub use arena::{__DynRootable, Mutation, R8Heap, Root, Rootable};
pub use collect::{__MustNotImplDrop, Collect, Relocate, Trace};
pub use context::Phase;
pub use gc::Gc;
pub use lock::{Lock, RefLock};
pub use metrics::Metrics;
pub use parallel::{Job, Pool};
pub use r8gc_macros::{__unelide_lifetimes, Collect};
pub use roots::{DynamicRoot, DynamicRootSet};
pub use static_wrapper::Static;
