use crate::ValueKind;
use alloc::boxed::Box;
use alloc::vec::Vec;
use core::fmt;

use crate::{Collect, Gc, GcCell, Mutation, ValueCell};

use crate::{Agent, Error, FuncId, Key, Value};

/// A string on the heap.
pub type JsString<'gc> = Gc<'gc, crate::text::JsStr>;

/// A Symbol: just a description; its identity is the pointer.
#[derive(Debug, Collect)]
#[collect(require_static)]
pub struct SymbolData {
    pub description: Option<Box<str>>,
}

/// A Symbol on the heap.
pub type JsSymbol<'gc> = Gc<'gc, SymbolData>;

/// A BigInt (arbitrary-precision integer).
#[derive(Debug, Collect)]
#[collect(require_static)]
pub struct BigIntData(pub num_bigint::BigInt);

/// A BigInt on the heap.
pub type JsBigInt<'gc> = Gc<'gc, BigIntData>;

/// The body of a RegExp object: source, flags, and what the regex engine compiled (type-erased, interpreted by
/// j8builtins).
#[derive(Debug, Collect)]
#[collect(no_drop)]
pub struct RegExpData<'gc> {
    pub source: JsString<'gc>,
    pub flags: JsString<'gc>,
    pub compiled: Gc<'gc, crate::Static<Box<dyn core::any::Any + Send + Sync>>>,
    /// The Annex B legacy feature (`compile`) is usable: only for objects created by %RegExp% itself (not subclass
    /// instances)
    pub legacy: bool,
}

/// An object on the heap (interior mutability through [`GcCell`]; writes go through paragc's write barrier).
pub type JsObject<'gc> = Gc<'gc, GcCell<'gc, ObjectData<'gc>>>;

/// A scope environment on the heap: bindings captured by inner functions live here, stored by slot.
pub type JsEnv<'gc> = Gc<'gc, Env<'gc>>;

/// A host function: takes `this` and arguments, returns a value or an exception.
pub type NativeFn = for<'gc, 'x> fn(&mut Agent<'gc, 'x>, Value<'gc>, &[Value<'gc>]) -> Result<Value<'gc>, Error>;

/// One scope environment layer; `parent` points to the outer one. The number of slots is fixed at creation; each
/// slot is an atomic cell, so any thread may read and write it.
#[derive(Debug, Collect)]
#[collect(no_drop)]
pub struct Env<'gc> {
    slots: Box<[ValueCell<'gc>]>,
    pub parent: Option<JsEnv<'gc>>,
}

impl<'gc> Env<'gc> {
    /// An environment holding `slots`
    pub fn new(slots: Vec<Value<'gc>>, parent: Option<JsEnv<'gc>>) -> Self {
        Self { slots: slots.into_iter().map(ValueCell::new).collect(), parent }
    }

    /// An environment of `n` slots, all `undefined`
    pub fn with_len(n: usize, parent: Option<JsEnv<'gc>>) -> Self {
        Self { slots: (0..n).map(|_| ValueCell::new(Value::Undefined)).collect(), parent }
    }

    pub fn alloc(self, mc: &Mutation<'gc>) -> JsEnv<'gc> {
        mc.alloc(self)
    }

    /// Slot `i`
    #[inline]
    pub fn load(&self, i: usize) -> Value<'gc> {
        self.slots[i].get()
    }

    /// The current values of every slot (a copy, e.g. for a per-iteration binding environment)
    pub fn values(&self) -> Vec<Value<'gc>> {
        self.slots.iter().map(ValueCell::get).collect()
    }

    /// Writes slot `i` of `env`
    #[inline]
    pub fn set(env: JsEnv<'gc>, mc: &Mutation<'gc>, i: usize, v: Value<'gc>) {
        env.get().slots[i].set(mc, v);
    }
}

/// A closure: function index, the environment at creation, the method's home object (`super` looks up from its
/// prototype), and the class constructor's instance field initializer.
#[derive(Clone, Copy, Debug, Collect)]
#[collect(no_drop)]
pub struct Closure<'gc> {
    pub func: FuncId,
    pub env: Option<JsEnv<'gc>>,
    pub home: Option<JsObject<'gc>>,
    pub fields: Option<JsObject<'gc>>,
}

impl<'gc> Closure<'gc> {
    pub fn new(func: FuncId, env: Option<JsEnv<'gc>>) -> Self {
        Self { func, env, home: None, fields: None }
    }
}

/// The contents of a property: data, or a pair of accessors.
#[derive(Clone, Copy, Debug, Collect)]
#[collect(no_drop)]
pub enum Slot<'gc> {
    Data(Value<'gc>),
    Accessor { get: Option<JsObject<'gc>>, set: Option<JsObject<'gc>> },
}

/// A property: contents plus attributes.
#[derive(Clone, Copy, Debug, Collect)]
#[collect(no_drop)]
pub struct Property<'gc> {
    pub slot: Slot<'gc>,
    pub enumerable: bool,
    pub writable: bool,
    pub configurable: bool,
}

impl<'gc> Property<'gc> {
    /// A property created by plain assignment: enumerable, writable, configurable.
    pub fn data(v: Value<'gc>) -> Self {
        Self { slot: Slot::Data(v), enumerable: true, writable: true, configurable: true }
    }

    /// A property of built-in objects and class methods: non-enumerable.
    pub fn hidden(v: Value<'gc>) -> Self {
        Self { enumerable: false, ..Self::data(v) }
    }

    pub fn value(&self) -> Option<Value<'gc>> {
        match self.slot {
            Slot::Data(v) => Some(v),
            Slot::Accessor { .. } => None,
        }
    }
}

/// The body of a host function object.
#[derive(Clone, Copy, Collect)]
#[collect(require_static)]
pub struct Native {
    pub f: NativeFn,
    /// Can be `new`ed (built-in constructor); when it can, [`Agent::new_target`] tells it whether this is a construct
    /// call
    pub constructor: bool,
    /// The execution layer may skip the host and expand it into a direct call to the target (`call` / `apply`)
    pub forward: Forward,
}

/// A built-in function that merely calls another function in a different way. The execution layer expands it into a
/// plain call, and the callee's JS stays on the execution layer's own frame stack (not reentered from inside a host
/// function, so it is still time-sliced and collected).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Forward {
    #[default]
    No,
    /// `Function.prototype.call`: `this` is the target, the first argument is the new `this`, the rest are passed
    /// through
    Call,
    /// `Function.prototype.apply`: `this` is the target, the first argument is the new `this`, the second is an
    /// array-like argument list
    Apply,
}

impl Native {
    /// A plain host function (cannot be `new`ed).
    pub fn function(f: NativeFn) -> Self {
        Self { f, constructor: false, forward: Forward::No }
    }

    /// A built-in constructor (can be `new`ed).
    pub fn constructor(f: NativeFn) -> Self {
        Self { f, constructor: true, forward: Forward::No }
    }
}

impl fmt::Debug for Native {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("Native")
    }
}

/// The result of `Function.prototype.bind`: calls are redirected to the target, with `this` and leading arguments
/// fixed.
#[derive(Debug, Collect)]
#[collect(no_drop)]
pub struct Bound<'gc> {
    pub target: Value<'gc>,
    pub this: Value<'gc>,
    pub args: Vec<Value<'gc>>,
}

/// What an array iterator yields.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Collect)]
#[collect(require_static)]
pub enum IterKind {
    Values,
    Keys,
    Entries,
}

/// The state of a built-in iterator.
#[derive(Debug, Collect)]
#[collect(no_drop)]
pub enum IteratorState<'gc> {
    /// An array (or array-like) walked by index; `target` being `None` means it is done
    Array { target: Option<Value<'gc>>, index: usize, kind: IterKind },
    /// A string walked by code point (`pos` is a UTF-8 byte position)
    String { s: JsString<'gc>, pos: usize },
}

/// A suspended frame: the full execution state a generator / async function saved at `yield` / `await`.
#[derive(Debug, Collect)]
#[collect(no_drop)]
pub struct Suspended<'gc> {
    pub func: FuncId,
    pub pc: usize,
    pub regs: Vec<Value<'gc>>,
    pub env: Option<JsEnv<'gc>>,
    /// Installed exception handlers: (landing pad, environment at install time)
    pub handlers: Vec<(u32, Option<JsEnv<'gc>>)>,
    pub args: Vec<Value<'gc>>,
    /// The register that receives the resume kind on resumption (absent before it starts running)
    pub mode: Option<u16>,
}

/// The state of a coroutine (generator / async function).
#[derive(Debug, Collect)]
#[collect(no_drop)]
pub enum CoroutineState<'gc> {
    /// Not started yet, or stopped at `yield` / `await`
    Suspended(Suspended<'gc>),
    /// Running (reentering `next` throws a TypeError)
    Running,
    Completed,
}

/// The coroutine of a generator or async function; an async function carries its result Promise.
#[derive(Debug, Collect)]
#[collect(no_drop)]
pub struct Coroutine<'gc> {
    pub state: CoroutineState<'gc>,
    /// Has not run its first step yet (at this point `return` / `throw` finish it directly)
    pub fresh: bool,
    pub promise: Option<JsObject<'gc>>,
    /// Async generator: queued `next` / `throw` / `return` requests; the head is the one being processed
    pub requests: Option<alloc::collections::VecDeque<Request<'gc>>>,
}

impl<'gc> Coroutine<'gc> {
    /// A coroutine that has not started: generator (neither `promise` nor `requests`), async function (with a result
    /// Promise), async generator (with a request queue).
    pub fn new(saved: Suspended<'gc>, promise: Option<JsObject<'gc>>, async_generator: bool) -> Self {
        Self { state: CoroutineState::Suspended(saved), fresh: true, promise, requests: async_generator.then(alloc::collections::VecDeque::new) }
    }
}

/// One request to an async generator: how to resume, the value sent in, and the Promise for the caller.
#[derive(Clone, Copy, Debug, Collect)]
#[collect(no_drop)]
pub struct Request<'gc> {
    pub mode: crate::Resume,
    pub value: Value<'gc>,
    pub promise: JsObject<'gc>,
}

/// The state of a Promise.
#[derive(Clone, Copy, Debug, Collect)]
#[collect(no_drop)]
pub enum Settled<'gc> {
    Pending,
    Fulfilled(Value<'gc>),
    Rejected(Value<'gc>),
}

/// What to do once a Promise settles.
#[derive(Clone, Copy, Debug, Collect)]
#[collect(no_drop)]
pub enum Reaction<'gc> {
    /// `then(onFulfilled, onRejected)`; the result goes to the capability record's resolve / reject (dropped if absent)
    Then { on_fulfilled: Value<'gc>, on_rejected: Value<'gc>, capability: Option<Capability<'gc>> },
    /// `await`: resume this coroutine
    Await { coroutine: JsObject<'gc> },
}

/// PromiseCapability: the result Promise and its resolve / reject functions.
#[derive(Clone, Copy, Debug, Collect)]
#[collect(no_drop)]
pub struct Capability<'gc> {
    pub promise: Value<'gc>,
    pub resolve: Value<'gc>,
    pub reject: Value<'gc>,
}

/// A microtask.
#[derive(Clone, Copy, Debug, Collect)]
#[collect(no_drop)]
pub enum Job<'gc> {
    /// A reaction: `value` is the settled value, `rejected` says whether it was a rejection
    Reaction { reaction: Reaction<'gc>, value: Value<'gc>, rejected: bool },
    /// Resolve a Promise with a thenable: call `then.call(thenable, resolve, reject)`
    Thenable { promise: JsObject<'gc>, thenable: Value<'gc>, then: Value<'gc> },
}

/// The key Map / Set use to find keys by SameValueZero: only one `NaN`, `-0` treated as `+0`, strings and BigInts
/// compared by content, objects and Symbols by identity.
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum MapKey {
    Undefined,
    Null,
    Bool(bool),
    Number(u64),
    /// The string value itself (short strings inline, long strings shared; no allocation on lookup)
    String(SmallStr),
    BigInt(Box<str>),
    /// The address of an object / Symbol (collection moves objects: after a move `Collection` rebuilds its index)
    Ref(usize),
}

impl From<Value<'_>> for MapKey {
    fn from(v: Value<'_>) -> Self {
        match v.kind() {
            ValueKind::Undefined | ValueKind::Uninit => MapKey::Undefined,
            ValueKind::Null => MapKey::Null,
            ValueKind::Bool(b) => MapKey::Bool(b),
            ValueKind::Number(x) if x.is_nan() => MapKey::Number(f64::NAN.to_bits()),
            ValueKind::Number(x) => MapKey::Number((x + 0.0).to_bits()),
            ValueKind::String(s) => MapKey::String(s.text().clone()),
            ValueKind::BigInt(b) => MapKey::BigInt(alloc::format!("{}", b.0).into()),
            ValueKind::Symbol(s) => MapKey::Ref(Gc::as_ptr(s) as usize),
            ValueKind::Object(o) => MapKey::Ref(Gc::as_ptr(o).cast::<u8>() as usize),
        }
    }
}

/// Map / Set storage: entries in insertion order (deleted ones are left as holes, so iteration can continue across
/// deletions and modifications) plus an index from key to position. The index finds keys by object address and is
/// rebuilt after collection moves keys (hand-written `Collect`).
#[derive(Debug)]
pub struct Collection<'gc> {
    entries: Vec<Option<(Value<'gc>, Value<'gc>)>>,
    index: crate::Static<hashbrown::HashMap<MapKey, usize, core::hash::BuildHasherDefault<rustc_hash::FxHasher>>>,
    pub size: usize,
    pub weak: bool,
}

// SAFETY: every entry is traced; moved keys get their new address in the index
unsafe impl crate::Trace for Collection<'_> {
    fn trace(&self, t: &mut crate::Tracer<'_>) {
        self.entries.trace(t);
    }

    /// Only moved keys (objects / Symbols, indexed by address) get their new address in the index. Pre-move addresses
    /// (blocks being evacuated) never overlap post-move ones, so delete-then-insert never hits another key.
    fn relocate(&mut self, t: &mut crate::Tracer<'_>) {
        for (i, entry) in self.entries.iter_mut().enumerate() {
            let Some((k, v)) = entry else { continue };
            let old = matches!(k.kind(), ValueKind::Object(_) | ValueKind::Symbol(_)).then(|| MapKey::from(*k));
            crate::Trace::relocate(k, t);
            crate::Trace::relocate(v, t);
            if let Some(old) = old {
                let new = MapKey::from(*k);
                if new != old {
                    self.index.0.remove(&old);
                    self.index.0.insert(new, i);
                }
            }
        }
    }
}

impl<'gc> Collection<'gc> {
    pub fn new(weak: bool) -> Self {
        Self { entries: Vec::new(), index: crate::Static(hashbrown::HashMap::default()), size: 0, weak }
    }

    /// Entries in insertion order (deleted ones are holes)
    pub fn entries(&self) -> &[Option<(Value<'gc>, Value<'gc>)>] {
        &self.entries
    }

    pub fn get(&self, key: Value<'gc>) -> Option<Value<'gc>> {
        let i = *self.index.0.get(&MapKey::from(key))?;
        self.entries[i].map(|(_, v)| v)
    }

    pub fn has(&self, key: Value<'gc>) -> bool {
        self.index.0.contains_key(&MapKey::from(key))
    }

    /// Set a value: update it if present, otherwise append at the end. A `-0` key is stored as `+0`.
    pub fn set(&mut self, key: Value<'gc>, value: Value<'gc>) {
        // `-0 + 0` is `+0`
        let key = match key.kind() {
            ValueKind::Number(x) => Value::Number(x + 0.0),
            k => k.into(),
        };
        // Hash only once
        match self.index.0.entry(MapKey::from(key)) {
            hashbrown::hash_map::Entry::Occupied(e) => {
                let i = *e.get();
                self.entries[i] = Some((key, value));
                
            }
            hashbrown::hash_map::Entry::Vacant(e) => {
                e.insert(self.entries.len());
                self.entries.push(Some((key, value)));
                self.size += 1;
            }
        }
    }

    pub fn delete(&mut self, key: Value<'gc>) -> bool {
        match self.index.0.remove(&MapKey::from(key)) {
            Some(i) => {
                self.entries[i] = None;
                self.size -= 1;
                true
            }
            None => false,
        }
    }

    /// Clear: every entry becomes a hole; an iterator in progress stops when it reaches the end.
    pub fn clear(&mut self) {
        self.entries.iter_mut().for_each(|e| *e = None);
        self.index.0.clear();
        self.size = 0;
    }
}

/// A host function with state: receives itself when called (the state is in `slots`).
pub type ClosureFn = for<'gc, 'x> fn(&mut Agent<'gc, 'x>, JsObject<'gc>, Value<'gc>, &[Value<'gc>]) -> Result<Value<'gc>, Error>;

/// The body of a stateful host function object (Promise element functions, Proxy revoke functions, etc.).
#[derive(Collect)]
#[collect(no_drop)]
pub struct NativeClosure<'gc> {
    #[collect(require_static)]
    pub f: ClosureFn,
    pub slots: Vec<Value<'gc>>,
}

impl fmt::Debug for NativeClosure<'_> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("NativeClosure").field("slots", &self.slots.len()).finish()
    }
}

/// Proxy: target and handler (both `None` after revocation); whether it is callable / constructible is fixed at
/// creation from the target.
#[derive(Clone, Copy, Debug, Collect)]
#[collect(no_drop)]
pub struct ProxyData<'gc> {
    pub target: Option<JsObject<'gc>>,
    pub handler: Option<JsObject<'gc>>,
    pub callable: bool,
    pub constructor: bool,
}

/// The kind of an Iterator helper (`map` / `filter` / `take` / `drop` / `flatMap`).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Collect)]
#[collect(require_static)]
pub enum HelperKind {
    Map,
    Filter,
    Take,
    Drop,
    FlatMap,
}

/// The state of an Iterator helper (generator-style).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Collect)]
#[collect(require_static)]
pub enum HelperState {
    Start,
    Suspended,
    Running,
    Done,
}

/// Iterator helper: the underlying iterator, the callback, the count left for `take` / `drop`, a counter, and the inner
/// iterator `flatMap` is walking.
#[derive(Clone, Copy, Debug, Collect)]
#[collect(no_drop)]
pub struct IteratorHelper<'gc> {
    pub kind: HelperKind,
    pub state: HelperState,
    pub underlying: crate::IterRecord<'gc>,
    pub f: Value<'gc>,
    pub remaining: f64,
    pub counter: f64,
    pub inner: Option<crate::IterRecord<'gc>>,
}

/// The element type of a TypedArray.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Collect)]
#[collect(require_static)]
pub enum ElementKind {
    Int8,
    Uint8,
    Uint8Clamped,
    Int16,
    Uint16,
    Int32,
    Uint32,
    Float16,
    Float32,
    Float64,
    BigInt64,
    BigUint64,
}

impl ElementKind {
    pub const ALL: [ElementKind; 12] = [
        ElementKind::Int8,
        ElementKind::Uint8,
        ElementKind::Uint8Clamped,
        ElementKind::Int16,
        ElementKind::Uint16,
        ElementKind::Int32,
        ElementKind::Uint32,
        ElementKind::Float16,
        ElementKind::Float32,
        ElementKind::Float64,
        ElementKind::BigInt64,
        ElementKind::BigUint64,
    ];

    /// Bytes per element.
    pub fn size(self) -> usize {
        match self {
            ElementKind::Int8 | ElementKind::Uint8 | ElementKind::Uint8Clamped => 1,
            ElementKind::Int16 | ElementKind::Uint16 | ElementKind::Float16 => 2,
            ElementKind::Int32 | ElementKind::Uint32 | ElementKind::Float32 => 4,
            ElementKind::Float64 | ElementKind::BigInt64 | ElementKind::BigUint64 => 8,
        }
    }

    /// Constructor name.
    pub fn name(self) -> &'static str {
        match self {
            ElementKind::Int8 => "Int8Array",
            ElementKind::Uint8 => "Uint8Array",
            ElementKind::Uint8Clamped => "Uint8ClampedArray",
            ElementKind::Int16 => "Int16Array",
            ElementKind::Uint16 => "Uint16Array",
            ElementKind::Int32 => "Int32Array",
            ElementKind::Uint32 => "Uint32Array",
            ElementKind::Float16 => "Float16Array",
            ElementKind::Float32 => "Float32Array",
            ElementKind::Float64 => "Float64Array",
            ElementKind::BigInt64 => "BigInt64Array",
            ElementKind::BigUint64 => "BigUint64Array",
        }
    }

    /// The type in DataView method names (`Int8` in `getInt8`).
    pub fn view_name(self) -> &'static str {
        match self {
            ElementKind::Int8 => "Int8",
            ElementKind::Uint8 | ElementKind::Uint8Clamped => "Uint8",
            ElementKind::Int16 => "Int16",
            ElementKind::Uint16 => "Uint16",
            ElementKind::Int32 => "Int32",
            ElementKind::Uint32 => "Uint32",
            ElementKind::Float16 => "Float16",
            ElementKind::Float32 => "Float32",
            ElementKind::Float64 => "Float64",
            ElementKind::BigInt64 => "BigInt64",
            ElementKind::BigUint64 => "BigUint64",
        }
    }

    pub fn is_bigint(self) -> bool {
        matches!(self, ElementKind::BigInt64 | ElementKind::BigUint64)
    }
}

/// The body of an ArrayBuffer / SharedArrayBuffer: bytes (absent once detached), the resizable maximum, and whether it
/// is shared.
#[derive(Debug, Collect)]
#[collect(require_static)]
pub struct BufferData {
    pub bytes: Option<Vec<u8>>,
    pub max: Option<usize>,
    pub shared: bool,
}

/// The range a TypedArray / DataView views: its buffer, offset, and length (`None` tracks the buffer's length).
#[derive(Clone, Copy, Debug, Collect)]
#[collect(no_drop)]
pub struct ViewData<'gc> {
    pub buffer: JsObject<'gc>,
    pub offset: usize,
    pub length: Option<usize>,
}

/// Mapped arguments: the environment holding the parameters and the slot each index maps to (no longer mapped once
/// deleted / turned into an accessor).
#[derive(Clone, Debug, Collect)]
#[collect(no_drop)]
pub struct ArgumentsMap<'gc> {
    pub env: JsEnv<'gc>,
    pub slots: Vec<Option<u16>>,
}

/// Rust data inside a host object (retrieved by type; mutable borrows via `RefCell`).
pub type HostData<'gc> = Gc<'gc, crate::Static<j8lockfree::AtomicRefCell<Box<dyn core::any::Any + Send + Sync>>>>;

/// The kind of an object: internal slots carried beyond those of an ordinary object.
#[derive(Debug, Default, Collect)]
#[collect(no_drop)]
pub enum ObjectKind<'gc> {
    #[default]
    Ordinary,
    /// An arguments object (recognized by `Object.prototype.toString`); for sloppy-mode functions with simple
    /// parameters, index i maps to the parameter's environment slot
    Arguments(Option<ArgumentsMap<'gc>>),
    /// An Error instance (has [[ErrorData]])
    Error,
    /// Array elements (index properties live here; `length` is its length)
    Array(Vec<Value<'gc>>),
    Function(Closure<'gc>),
    Native(Native),
    Bound(Box<Bound<'gc>>),
    Iterator(IteratorState<'gc>),
    Coroutine(Box<Coroutine<'gc>>),
    Promise { state: Settled<'gc>, reactions: Vec<Reaction<'gc>> },
    /// Promise resolve / reject functions: share one "already resolved" flag
    Resolver { promise: JsObject<'gc>, reject: bool, done: Gc<'gc, core::sync::atomic::AtomicBool> },
    RegExp(RegExpData<'gc>),
    /// Primitive wrapper object (`new Boolean(…)` / `new Number(…)` / `new String(…)` / `Object(sym)` …): holds the
    /// wrapped primitive
    Primitive(Value<'gc>),
    /// Map (a WeakMap when `weak` is true)
    Map(Box<Collection<'gc>>),
    /// Set (values stored as both key and value; a WeakSet when `weak` is true)
    Set(Box<Collection<'gc>>),
    /// Map / Set iterator: a live view walking by entry index (`target` being `None` means it is done)
    CollectionIterator { target: Option<JsObject<'gc>>, index: usize, kind: IterKind, set: bool },
    NativeClosure(NativeClosure<'gc>),
    Proxy(ProxyData<'gc>),
    IteratorHelper(Box<IteratorHelper<'gc>>),
    /// An iterator wrapped by `Iterator.from`
    WrappedIterator(crate::IterRecord<'gc>),
    ArrayBuffer(Box<BufferData>),
    /// Date: the time value (milliseconds; NaN when invalid)
    Date(f64),
    /// Host object: Rust data put here by the binding layer (j8binding) (no GC pointers; retrieved by type)
    Host(HostData<'gc>),
    /// Module namespace object: export names (sorted) and the bindings they resolve to
    ModuleNamespace(Vec<(JsString<'gc>, crate::ResolvedBinding<'gc>)>),
    /// WeakRef (currently holds its target strongly)
    WeakRef(Value<'gc>),
    /// FinalizationRegistry: the cleanup callback and registrations of (target, held value, unregister token)
    /// (currently strong; cleanup never fires)
    FinalizationRegistry { cleanup: Value<'gc>, cells: Vec<(Value<'gc>, Value<'gc>, Value<'gc>)> },
    /// The `matchAll` iterator: matcher, string, whether it has `g`, whether it walks by code point, whether it is done
    RegExpStringIterator { matcher: JsObject<'gc>, string: JsString<'gc>, global: bool, unicode: bool, done: bool },
    /// TypedArray: `length` is the element count
    TypedArray(ElementKind, ViewData<'gc>),
    /// DataView: `length` is the byte count
    DataView(ViewData<'gc>),
}

impl ObjectKind<'_> {
    /// Own properties are exactly those in the property table: [[GetOwnProperty]] / [[GetPrototypeOf]] are both
    /// ordinary (not a Proxy / array / typed array / module namespace / string wrapper / arguments), so property lookup
    /// can take the fast path.
    pub fn has_ordinary_properties(&self) -> bool {
        !(matches!(self, ObjectKind::Proxy(_) | ObjectKind::Array(_) | ObjectKind::TypedArray(..) | ObjectKind::ModuleNamespace(_) | ObjectKind::Arguments(_))
            || matches!(self, ObjectKind::Primitive(v) if matches!(v.kind(), ValueKind::String(_))))
    }

    pub fn is_callable(&self) -> bool {
        matches!(self, ObjectKind::Function(_) | ObjectKind::Native(_) | ObjectKind::Bound(_) | ObjectKind::Resolver { .. } | ObjectKind::NativeClosure(_) | ObjectKind::Proxy(ProxyData { callable: true, .. }))
    }
}


pub use crate::text::{JsStr, SmallStr, Units};

/// A set of things most objects don't have (Symbol-keyed properties, private members): takes a single pointer when
/// empty.
#[derive(Collect)]
#[collect(no_drop)]
#[expect(clippy::box_collection, reason = "boxed so an empty one is a single pointer (a Vec itself is three words)")]
pub struct Few<T>(Option<Box<Vec<T>>>);

impl<T> Default for Few<T> {
    fn default() -> Self {
        Self(None)
    }
}

impl<T> core::ops::Deref for Few<T> {
    type Target = [T];

    fn deref(&self) -> &[T] {
        self.0.as_deref().map_or(&[], |v| v)
    }
}

impl<T> core::ops::DerefMut for Few<T> {
    fn deref_mut(&mut self) -> &mut [T] {
        self.0.as_deref_mut().map_or(&mut [], |v| v)
    }
}

impl<T> Few<T> {
    pub fn push(&mut self, t: T) {
        self.0.get_or_insert_default().push(t);
    }

    pub fn remove(&mut self, i: usize) -> T {
        self.0.as_mut().expect("an index into a non-empty list").remove(i)
    }

    pub fn retain(&mut self, keep: impl FnMut(&T) -> bool) {
        if let Some(v) = &mut self.0 {
            v.retain(keep);
        }
    }

    pub fn pop(&mut self) -> Option<T> {
        self.0.as_mut()?.pop()
    }

    pub fn into_vec(self) -> Vec<T> {
        self.0.map_or_else(Vec::new, |v| *v)
    }
}

impl<T> From<Vec<T>> for Few<T> {
    fn from(v: Vec<T>) -> Self {
        Self((!v.is_empty()).then(|| Box::new(v)))
    }
}

/// An object: kind, property table in insertion order, prototype.
#[derive(Default, Collect)]
#[collect(no_drop)]
pub struct ObjectData<'gc> {
    pub kind: ObjectKind<'gc>,
    /// String-keyed properties: shape (hidden class) plus slots, in insertion order
    pub props: crate::Props<'gc>,
    /// The empty shape that objects with this object as prototype start from (so objects with the same prototype that
    /// add properties the same way share shapes). Not a `Gc`, so changing it needs no write barrier
    #[collect(require_static)]
    pub shape_root: j8lockfree::OnceBox<alloc::sync::Arc<crate::Shape>>,
    /// Symbol-keyed properties
    pub symbols: Few<(JsSymbol<'gc>, Property<'gc>)>,
    /// Private members (`#x`): keys are private names generated when the class is evaluated; kept apart from ordinary
    /// properties and invisible to reflection
    pub privates: Few<(JsSymbol<'gc>, Property<'gc>)>,
    pub proto: Option<JsObject<'gc>>,
    /// Array: the length extended by the sparse part. Indices outside the dense part (in `ObjectKind::Array`: all
    /// writable, enumerable, configurable data properties with no holes) are stored as ordinary properties; the actual
    /// length is the larger of this and the dense part's length
    pub sparse_length: u32,
    /// Not extensible (`Object.preventExtensions` / `seal` / `freeze`)
    pub non_extensible: bool,
    /// The array's `length` is not writable
    pub length_readonly: bool,
    /// The property table has had a key starting with a digit (set only, never cleared; array method fast paths rely on
    /// it to confirm at a glance that the prototype has no index properties)
    pub digit_keys: bool,
}

/// Prints only kind and keys: the object graph has cycles (`F.prototype.constructor`), so it cannot be expanded.
impl fmt::Debug for ObjectData<'_> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let kind = match self.kind {
            ObjectKind::Ordinary | ObjectKind::Arguments(_) => "Object",
            ObjectKind::Error => "Error",
            ObjectKind::Array(_) => "Array",
            ObjectKind::Function(_) | ObjectKind::Native(_) | ObjectKind::Bound(_) | ObjectKind::Resolver { .. } | ObjectKind::NativeClosure(_) => "Function",
            ObjectKind::Proxy(_) => "Proxy",
            ObjectKind::ArrayBuffer(_) => "ArrayBuffer",
            ObjectKind::Date(_) => "Date",
            ObjectKind::Host(_) => "Host",
            ObjectKind::ModuleNamespace(_) => "Module",
            ObjectKind::WeakRef(_) => "WeakRef",
            ObjectKind::FinalizationRegistry { .. } => "FinalizationRegistry",
            ObjectKind::RegExpStringIterator { .. } => "RegExp String Iterator",
            ObjectKind::TypedArray(..) => "TypedArray",
            ObjectKind::DataView(_) => "DataView",
            ObjectKind::Iterator(_) | ObjectKind::CollectionIterator { .. } | ObjectKind::IteratorHelper(_) | ObjectKind::WrappedIterator(_) => "Iterator",
            ObjectKind::Coroutine(_) => "Generator",
            ObjectKind::Promise { .. } => "Promise",
            ObjectKind::RegExp(_) => "RegExp",
            ObjectKind::Primitive(_) => "Primitive",
            ObjectKind::Map(_) => "Map",
            ObjectKind::Set(_) => "Set",
        };
        f.debug_list().entries(self.props.iter().map(|(k, _)| &**k)).finish()?;
        f.write_str(kind)
    }
}

impl<'gc> ObjectData<'gc> {
    /// Array length; `None` if not an array.
    pub fn array_length(&self) -> Option<u32> {
        match &self.kind {
            ObjectKind::Array(items) => Some(u32::try_from(items.len()).unwrap_or(u32::MAX).max(self.sparse_length)),
            _ => None,
        }
    }

    /// Fast path: update the existing `i`-th element of the dense part; returns `true` if updated (other cases take the
    /// full property write).
    pub fn write_index(&mut self, i: u32, value: Value<'gc>) -> bool {
        match &mut self.kind {
            ObjectKind::Array(items) if (i as usize) < items.len() => {
                items[i as usize] = value;
                true
            }
            _ => false,
        }
    }

    /// Move the dense part from element `from` onward into ordinary index-keyed properties (when those elements need
    /// different attributes, or one of them is deleted).
    pub fn spill(&mut self, from: usize) {
        let ObjectKind::Array(items) = &mut self.kind else { return };
        if from >= items.len() {
            return;
        }
        let len = u32::try_from(items.len()).unwrap_or(u32::MAX);
        let moved: Vec<Value<'gc>> = items.drain(from..).collect();
        for (i, v) in (from..).zip(moved) {
            self.push_prop(&alloc::format!("{i}"), Property::data(v));
        }
        self.sparse_length = self.sparse_length.max(len);
    }

    /// Elements of the array's sparse part (indices after the dense part, stored as ordinary properties), sorted by
    /// index.
    pub fn sparse_elements(&self) -> Vec<(u32, Value<'gc>)> {
        let ObjectKind::Array(items) = &self.kind else { return Vec::new() };
        if self.sparse_length as usize <= items.len() {
            return Vec::new();
        }
        let mut out: Vec<(u32, Value<'gc>)> = self
            .props
            .iter()
            .filter_map(|(k, p)| match (crate::ops::index_of_str(k), p.slot) {
                (Some(i), Slot::Data(v)) if i as usize >= items.len() => Some((i, v)),
                _ => None,
            })
            .collect();
        out.sort_unstable_by_key(|&(i, _)| i);
        out
    }

    /// Set an array's length: truncate elements at `n` and beyond (including index-keyed ones); growing only records
    /// the length (new positions are holes).
    pub fn set_array_length(&mut self, n: u32) {
        let ObjectKind::Array(items) = &mut self.kind else { return };
        let len = n as usize;
        if len <= items.len() {
            items.truncate(len);
        }
        let dense = items.len();
        if self.sparse_length > 0 {
            self.props.retain(|k| crate::ops::index_of_str(k).is_none_or(|i| i < n));
        }
        self.sparse_length = if len > dense { n } else { 0 };
    }

    /// Own property (only what is stored: the array dense part, ordinary properties, Symbol keys; array `length` and
    /// string indices are supplied by Agent).
    pub fn own_slot(&self, key: Key<'_, 'gc>) -> Option<Property<'gc>> {
        match key {
            Key::Str(k) => {
                // Check for an array before parsing the index: ordinary objects don't need to parse to look up a
                // property
                if let ObjectKind::Array(items) = &self.kind
                    && let Some(i) = crate::ops::index_of_str(k)
                    && let Some(&v) = items.get(i as usize)
                {
                    return Some(Property::data(v));
                }
                self.get_own(k)
            }
            Key::Sym(s) => self.get_own_symbol(s),
        }
    }

    /// Store an own property (already validated as storable): an array index goes into the dense part if it can,
    /// otherwise it is stored as an ordinary property and extends the length. Fast path for ordinary-object property
    /// writes (the OrdinarySet step on the receiver itself): an own writable data property gets its value updated; if
    /// absent and extensible, a new ordinary data property is created. Returns `Some(succeeded)`; returns `None` when
    /// the slow path is needed (accessor / not an ordinary object).
    pub fn ordinary_put(&mut self, key: Key<'_, 'gc>, value: Value<'gc>) -> Option<bool> {
        if !self.kind.has_ordinary_properties() {
            return None;
        }
        let found = match key {
            Key::Str(k) => self.props.position(k).map(|i| (self.props.attrs(i).writable(), self.props.slot_mut(i))),
            Key::Sym(s) => self.symbols.iter_mut().find(|(k, _)| Gc::ptr_eq(*k, s)).map(|(_, p)| (p.writable, &mut p.slot)),
        };
        match found {
            Some((true, Slot::Data(v))) => {
                *v = value;
                Some(true)
            }
            Some((false, Slot::Data(_))) => Some(false),
            Some(_) => None,
            None if self.non_extensible => Some(false),
            None => {
                self.store(key, Property::data(value));
                Some(true)
            }
        }
    }

    pub fn store(&mut self, key: Key<'_, 'gc>, prop: Property<'gc>) {
        let k = match key {
            Key::Sym(s) => return self.define_symbol(s, prop),
            Key::Str(k) => k,
        };
        let plain = prop.enumerable && prop.writable && prop.configurable;
        let stored = matches!(self.kind, ObjectKind::Array(_)) && self.get_own(k).is_some();
        if let (ObjectKind::Array(items), Some(i)) = (&mut self.kind, crate::ops::index_of_str(k)) {
            let at = i as usize;
            match (prop.slot, plain) {
                (Slot::Data(v), true) if at < items.len() => {
                    items[at] = v;
                    return;
                }
                (Slot::Data(v), true) if at == items.len() && !stored => {
                    items.push(v);
                    return;
                }
                _ if at < items.len() => self.spill(at),
                _ => {}
            }
        }
        self.define(k, prop);
        if let (ObjectKind::Array(_), Some(i)) = (&self.kind, crate::ops::index_of_str(k)) {
            self.sparse_length = self.sparse_length.max(i.saturating_add(1));
        }
    }

    /// Delete an own property (configurability not checked).
    pub fn unstore(&mut self, key: Key<'_, 'gc>) {
        match key {
            Key::Sym(s) => self.symbols.retain(|(k, _)| !Gc::ptr_eq(*k, s)),
            Key::Str(k) => {
                if let (ObjectKind::Array(items), Some(i)) = (&self.kind, crate::ops::index_of_str(k))
                    && (i as usize) < items.len()
                {
                    self.spill(i as usize);
                }
                if let Some(i) = self.position(k) {
                    self.props.remove(i);
                }
            }
        }
    }

    /// The index of a property in the table.
    /// Returns it if entry `at` of the property table is exactly `key` (the execution layer caches the index and checks
    /// it before use).
    pub fn prop_at(&self, at: usize, key: &str) -> Option<Property<'gc>> {
        (at < self.props.len() && **self.props.key(at) == *key).then(|| self.props.get(at))
    }

    /// Update the value of entry `at` of the property table (the caller has already checked with
    /// [`prop_at`](Self::prop_at) that it is a writable data property).
    pub fn set_at(&mut self, at: usize, value: Value<'gc>) {
        if at < self.props.len() {
            *self.props.slot_mut(at) = Slot::Data(value);
        }
    }

    pub fn position(&self, key: &str) -> Option<usize> {
        self.props.position(key)
    }

    /// Append an entry (the shape follows the transition table).
    fn push_prop(&mut self, key: &str, prop: Property<'gc>) {
        self.digit_keys |= key.as_bytes().first().is_some_and(u8::is_ascii_digit);
        let proto = self.proto;
        self.props.push(key, prop, || proto.and_then(|p| p.try_borrow().map(|d| d.root_shape())));
    }

    /// The empty shape that objects with this object as prototype start from (created if absent)
    pub fn root_shape(&self) -> alloc::sync::Arc<crate::Shape> {
        self.shape_root.get_or_init(crate::Props::empty).clone()
    }

    /// Append an entry, switching the shape directly to `to` (given by the execution layer's inline cache: adding one
    /// key to the current shape yields it, and the new key is its last one).
    pub fn push_to(&mut self, to: alloc::sync::Arc<crate::Shape>, slot: Slot<'gc>) {
        self.digit_keys |= to.key(to.len() - 1).as_bytes().first().is_some_and(u8::is_ascii_digit);
        self.props.push_to(to, slot);
    }

    /// Own property.
    pub fn get_own(&self, key: &str) -> Option<Property<'gc>> {
        self.position(key).map(|i| self.props.get(i))
    }

    /// The value of an own data property (`None` for accessors).
    pub fn own(&self, key: &str) -> Option<Value<'gc>> {
        self.get_own(key).and_then(|p| p.value())
    }

    /// Assignment: update the value of an existing data property (no-op if not writable), otherwise create an ordinary
    /// property. Accessors are handled by the caller first.
    pub fn set(&mut self, key: &str, value: Value<'gc>) {
        match self.position(key) {
            Some(i) => {
                if self.props.attrs(i).writable() {
                    *self.props.slot_mut(i) = Slot::Data(value);
                }
            }
            None => self.push_prop(key, Property::data(value)),
        }
    }

    /// Define: replace (or create) this property as a whole.
    pub fn define(&mut self, key: &str, prop: Property<'gc>) {
        match self.position(key) {
            Some(i) => self.props.replace(i, prop),
            None => self.push_prop(key, prop),
        }
    }

    pub fn get_own_symbol(&self, key: JsSymbol<'gc>) -> Option<Property<'gc>> {
        self.symbols.iter().find_map(|&(k, p)| Gc::ptr_eq(k, key).then_some(p))
    }

    pub fn define_symbol(&mut self, key: JsSymbol<'gc>, prop: Property<'gc>) {
        match self.symbols.iter_mut().find(|(k, _)| Gc::ptr_eq(*k, key)) {
            Some((_, p)) => *p = prop,
            None => self.symbols.push((key, prop)),
        }
    }

    pub fn remove_symbol(&mut self, key: JsSymbol<'gc>) -> bool {
        match self.symbols.iter().position(|(k, _)| Gc::ptr_eq(*k, key)) {
            Some(i) if self.symbols[i].1.configurable => {
                self.symbols.remove(i);
                true
            }
            Some(_) => false,
            None => true,
        }
    }

    /// Delete an own property: non-configurable ones can't be deleted; returns whether it was deleted (nonexistent
    /// counts as deleted).
    pub fn remove(&mut self, key: &str) -> bool {
        match self.position(key) {
            Some(i) if self.props.attrs(i).configurable() => {
                self.props.remove(i);
                true
            }
            Some(_) => false,
            None => true,
        }
    }

    pub fn alloc(self, mc: &Mutation<'gc>) -> JsObject<'gc> {
        mc.alloc(GcCell::new(mc, self))
    }
}

impl<'gc> From<ObjectKind<'gc>> for ObjectData<'gc> {
    fn from(kind: ObjectKind<'gc>) -> Self {
        Self { kind, ..Self::default() }
    }
}
