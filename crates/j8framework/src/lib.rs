//! j8framework: the core framework layer. It defines traits and common structs / enums; concrete implementations live
//! in the per-layer crates (e.g. j8platform implements [`Platform`]).
//!
//! The heap is [paragc](https://crates.io/crates/paragc) (wrapped in `gc.rs`): every JS value carries the branded lifetime
//! `'gc` and can only live within one [`Isolate::enter`].
//!
//! - [`Platform`]: threads and clocks (j8platform provides a standard library implementation)
//! - [`Isolate`]: an independent JS world, holding the GC heap and the profiler
//! - [`Scope`]: the scope of one entry into an isolate; a [`Value`] in it is V8's Local and becomes invisible once the
//!   scope is left
//! - [`Global`]: a persistent handle, surviving across multiple `enter` calls and released on drop
//! - [`Context`] / [`Realm`]: the execution context, with the global object and the built-in prototypes ([`Intrinsics`]
//!   is implemented by j8builtins)
//! - [`Agent`]: the context of one run; JS operation semantics hang off it, and calls back into JS go through
//!   [`Invoke`] (implemented by the execution engine)
//! - [`Profiler`]: call / back-edge counters
//! - [`Engine`]: the unified entry point implemented by the execution engine (j8executor); resumable
//!   ([`Engine::start`] / [`Engine::resume`])
//! - [`EventLoop`] / [`EventLoopSet`]: JS event loops (an isolate, its engine and a context each) and the loop threads
//!   they are pinned to, placed on the least loaded thread
extern crate alloc;
// `#[derive(Collect)]` 生成的代码写 `::j8framework::…`，本 crate 里也要能这样引用自己
extern crate self as j8framework;

mod agent;
mod bigint;
mod engine;
mod event_loop;
mod error;
mod gc;
mod handle;
mod isolate;
mod module;
mod object;
mod parallel;
mod ops;
mod platform;
mod private;
mod proxy;
mod typed;
mod profiler;
mod property;
mod promise;
mod realm;
mod shape;
mod store;
mod text;
mod value;

pub use agent::{Agent, CodeKind, Invoke, Resume, Step};
pub use engine::{Engine, ScriptKind, Source};
pub use event_loop::{EventLoop, EventLoopSet, Ticket, CHECK, SLICE};
pub use j8taskqueue::Quota;
pub use error::Error;
pub use handle::{ContextRoot, Global, RealmBrand, ValueBrand};
pub use isolate::{CreateParams, Isolate, Scope};
pub use object::{Accessor, ObjMut, ObjRef, Object, ObjectState, Few, ArgumentsMap, HostData, HostHook, HostReaction, BigIntData, Bound, BufferData, Capability, Closure, ClosureFn, ElementKind, ViewData, HelperKind, HelperState, IteratorHelper, NativeClosure, ProxyData, Collection, MapKey, Request, JsBigInt, RegExpData, Coroutine, CoroutineState, Env, IterKind, IteratorState, Job, JsEnv, JsObject, JsString, JsSymbol, Forward, Native, NativeFn, ObjectData, ObjectKind, JsStr, SmallStr, Units, Property, Reaction, Settled, Slot, Suspended, SymbolData};
pub use bigint::Numeric;
pub use ops::{number_text, BinaryOp, CompareOp, ErrorKind, Hint, IterRecord, UnaryOp};
pub use platform::{AsyncJob, Platform, Settle, Task};
pub use shape::{Attrs, Dict, Props, Shape};
pub use store::{Store, ValueArray};
pub use parallel::Par;
pub use platform::Pool;
pub use gc::{Mutable, CellExt, CellMut, CellRef, Collect, Cow, Erased, Gc, GcCell, GcQueue, Heap, Mutation, Mutator, Root, Slot as GcSlot, Brand, Static, Tlab, Trace, Tracer, ValueCell, erase, gc_from_raw};
pub use paragc;
pub use j8macros::heap;
pub use module::{ImportEntry, ImportName, ModuleInfo, ModuleRecord, ModuleStatus, Resolution, ResolvedBinding};
pub use profiler::{Counter, FuncId, Profiler};
pub use typed::{Element, canonical_numeric, f16_to_f64, f64_to_f16};
pub use property::{Descriptor, Integrity, Key, PropertyKey};
pub use realm::{Context, Intrinsics, Realm, RegExpCompiled, RegExpPristine, WellKnown};
pub use value::{Value, ValueBits, ValueKind, ValueTag};
