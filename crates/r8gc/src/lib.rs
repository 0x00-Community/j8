//! r8gc: a generational, moving (compacting), parallel GC for Rust with a `'gc` brand lifetime.
//!
//! - The young generation uses per-block bump allocation; a minor collection starts from the roots and the
//!   remembered set and copies live objects into the old generation in parallel (the write barrier maintains the
//!   remembered set)
//! - The old generation is paged by size class; a major collection marks incrementally (epoch flip, backward write
//!   barrier), and marking / sweeping / compaction all run in parallel on the thread pool
//! - The thread pool is a borrowed [`Pool`]: moremath's `ThreadMulti` (`ThreadSingle`) works directly, and several
//!   [`R8Heap`]s can share one
//! - Collection happens only outside [`R8Heap::mutate`], when every `Gc` is reachable from the roots, so objects
//!   can move: besides `trace`, [`Collect`] has `relocate`, which updates fields to the moved addresses
//!
//! The API follows the shape of gc-arena (`Gc` / `Lock` / `RefLock` / `Rootable!` / `DynamicRootSet`).
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
