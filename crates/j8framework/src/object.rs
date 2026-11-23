use crate::ValueKind;
use alloc::boxed::Box;
use alloc::vec::Vec;
use core::fmt;

use core::ops::{Deref, DerefMut};

use crate::{CellMut, CellRef, Collect, Gc, GcCell, GcSlot, Mutation, ValueCell};

use crate::{Agent, Backing, Error, FuncId, Key, Value};

pub type JsString<'gc> = Gc<'gc, crate::text::JsStr<'gc>>;

#[derive(Debug, Collect)]
#[collect(require_static)]
pub struct SymbolData {
    pub description: Option<Box<str>>,
    pub private: bool,
}

pub type JsSymbol<'gc> = Gc<'gc, SymbolData>;

#[derive(Debug, Collect)]
#[collect(require_static)]
pub struct BigIntData(pub num_bigint::BigInt);

pub type JsBigInt<'gc> = Gc<'gc, BigIntData>;

#[derive(Clone, Copy, Debug, Collect)]
#[collect(no_drop)]
pub struct RegExpData<'gc> {
    pub source: JsString<'gc>,
    pub flags: JsString<'gc>,
    pub compiled: Gc<'gc, crate::Static<Box<dyn core::any::Any + Send + Sync>>>,
    pub legacy: bool,
}

pub type JsObject<'gc> = Gc<'gc, Object<'gc>>;

pub type JsEnv<'gc> = Gc<'gc, Env<'gc>>;

pub type NativeFn = for<'gc, 'x> fn(&mut Agent<'gc, 'x>, Value<'gc>, &[Value<'gc>]) -> Result<Value<'gc>, Error>;

#[derive(Debug, Collect)]
#[collect(no_drop)]
pub struct Env<'gc> {
    slots: EnvSlots<'gc>,
    pub parent: Option<JsEnv<'gc>>,
    #[collect(require_static)]
    pub shape: Option<ShapeId>,
    pub extension: Option<JsObject<'gc>>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ShapeId(pub u32);

const INLINE_SLOTS: usize = 4;

#[derive(Debug, Collect)]
#[collect(no_drop)]
#[repr(C, u8)]
enum EnvSlots<'gc> {
    Inline { len: u8, cells: [ValueCell<'gc>; INLINE_SLOTS] },
    Boxed(Box<[ValueCell<'gc>]>),
}

impl<'gc> EnvSlots<'gc> {
    fn new(n: usize, mut init: impl FnMut(usize) -> Value<'gc>) -> Self {
        match u8::try_from(n) {
            Ok(len) if n <= INLINE_SLOTS => Self::Inline { len, cells: core::array::from_fn(|i| ValueCell::new(if i < n { init(i) } else { Value::Undefined })) },
            _ => Self::Boxed((0..n).map(|i| ValueCell::new(init(i))).collect()),
        }
    }

    #[inline]
    fn cells(&self) -> &[ValueCell<'gc>] {
        match self {
            Self::Inline { len, cells } => &cells[..usize::from(*len)],
            Self::Boxed(b) => b,
        }
    }
}

#[derive(Debug, Default)]
pub struct ContextShape {
    pub kind: ContextKind,
    pub names: Box<[ContextName]>,
}

#[derive(Debug)]
pub struct ContextName {
    pub name: SmallStr,
    pub constant: bool,
    pub lexical: bool,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum ContextKind {
    #[default]
    Function,
    Block,
    Catch,
    With,
    Script,
    Eval,
    Module,
}

impl ContextShape {
    pub fn slot(&self, name: &str) -> Option<(usize, &ContextName)> {
        self.names.iter().enumerate().find(|(_, n)| *n.name == *name)
    }
}

impl<'gc> Env<'gc> {
    pub const SLOTS_OFFSET: usize = core::mem::offset_of!(Env<'static>, slots);
    pub const PARENT_OFFSET: usize = core::mem::offset_of!(Env<'static>, parent);

    pub fn new(slots: Vec<Value<'gc>>, parent: Option<JsEnv<'gc>>) -> Self {
        let n = slots.len();
        let mut values = slots.into_iter();
        Self { slots: EnvSlots::new(n, |_| values.next().unwrap_or_default()), parent, shape: None, extension: None }
    }

    pub fn context(n: usize, init: Value<'gc>, parent: Option<JsEnv<'gc>>, shape: Option<ShapeId>, extension: Option<JsObject<'gc>>) -> Self {
        Self { slots: EnvSlots::new(n, |_| init), parent, shape, extension }
    }

    pub fn len(&self) -> usize {
        self.slots.cells().len()
    }

    pub fn is_empty(&self) -> bool {
        self.slots.cells().is_empty()
    }

    pub fn alloc(self, mc: &Mutation<'gc>) -> JsEnv<'gc> {
        mc.alloc(self)
    }

    #[inline]
    pub fn load(&self, i: usize) -> Value<'gc> {
        self.slots.cells()[i].get()
    }

    pub fn values(&self) -> Vec<Value<'gc>> {
        self.slots.cells().iter().map(ValueCell::get).collect()
    }

    #[inline]
    pub fn set(env: JsEnv<'gc>, mc: &Mutation<'gc>, i: usize, v: Value<'gc>) {
        env.get().slots.cells()[i].set(mc, v);
    }
}

#[derive(Clone, Copy, Debug, Collect)]
#[collect(no_drop)]
pub struct Closure<'gc> {
    pub func: FuncId,
    pub env: Option<JsEnv<'gc>>,
    #[collect(require_static)]
    pub code: Option<Code>,
}

impl<'gc> Closure<'gc> {
    pub fn new(func: FuncId, env: Option<JsEnv<'gc>>) -> Self {
        Self { func, env, code: None }
    }
}

#[derive(Debug)]
pub struct FunctionInfo {
    pub name: SmallStr,
    pub length: u16,
}

#[derive(Clone, Copy, Debug)]
pub struct Code(core::ptr::NonNull<FunctionInfo>);

unsafe impl Send for Code {}

unsafe impl Sync for Code {}

impl Code {
    pub fn new(info: &FunctionInfo) -> Self {
        Self(core::ptr::NonNull::from(info))
    }

    #[inline]
    pub unsafe fn info<'e>(self) -> &'e FunctionInfo {
        unsafe { self.0.as_ref() }
    }

    pub unsafe fn get<'e, T>(self) -> &'e T {
        unsafe { self.0.cast::<T>().as_ref() }
    }
}

#[derive(Clone, Copy, Debug, Collect)]
#[collect(no_drop)]
pub enum Slot<'gc> {
    Data(Value<'gc>),
    Accessor { get: Option<JsObject<'gc>>, set: Option<JsObject<'gc>> },
}

#[derive(Clone, Copy, Debug, Collect)]
pub struct Accessor<'gc> {
    pub get: Option<JsObject<'gc>>,
    pub set: Option<JsObject<'gc>>,
}

impl<'gc> Slot<'gc> {
    pub fn encode(self, mc: &Mutation<'gc>) -> Value<'gc> {
        match self {
            Slot::Data(v) => v,
            Slot::Accessor { get, set } => Value::accessor(mc.alloc(Accessor { get, set })),
        }
    }

    pub fn decode(v: Value<'gc>) -> Self {
        match v.as_accessor() {
            Some(a) => Slot::Accessor { get: a.get().get, set: a.get().set },
            None => Slot::Data(v),
        }
    }
}

#[derive(Clone, Copy, Debug, Collect)]
#[collect(no_drop)]
pub struct Property<'gc> {
    pub slot: Slot<'gc>,
    pub enumerable: bool,
    pub writable: bool,
    pub configurable: bool,
}

impl<'gc> Property<'gc> {
    pub fn data(v: Value<'gc>) -> Self {
        Self { slot: Slot::Data(v), enumerable: true, writable: true, configurable: true }
    }

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

#[derive(Clone, Copy, Collect)]
#[collect(require_static)]
pub struct Native {
    pub f: NativeFn,
    pub constructor: bool,
    pub forward: Forward,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Forward {
    #[default]
    No,
    Call,
    Apply,
}

impl Native {
    pub fn function(f: NativeFn) -> Self {
        Self { f, constructor: false, forward: Forward::No }
    }

    pub fn constructor(f: NativeFn) -> Self {
        Self { f, constructor: true, forward: Forward::No }
    }
}

impl fmt::Debug for Native {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("Native")
    }
}

#[derive(Debug, Collect)]
#[collect(no_drop)]
pub struct Bound<'gc> {
    pub target: Value<'gc>,
    pub this: Value<'gc>,
    pub args: Vec<Value<'gc>>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Collect)]
#[collect(require_static)]
pub enum IterKind {
    Values,
    Keys,
    Entries,
}

#[derive(Debug, Collect)]
#[collect(no_drop)]
pub enum IteratorState<'gc> {
    Array { target: Option<Value<'gc>>, index: usize, kind: IterKind },
    String { s: JsString<'gc>, pos: usize },
}

#[derive(Debug, Collect)]
#[collect(no_drop)]
pub struct Suspended<'gc> {
    pub closure: Value<'gc>,
    pub params: Vec<Value<'gc>>,
    pub regs: Vec<Value<'gc>>,
    pub context: Option<JsEnv<'gc>>,
    pub continuation: u32,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Collect)]
#[collect(require_static)]
pub enum CoroutineState {
    Suspended,
    Running,
    Completed,
}

#[derive(Debug, Collect)]
#[collect(no_drop)]
pub struct Coroutine<'gc> {
    pub state: CoroutineState,
    pub frame: Suspended<'gc>,
    pub mode: crate::Resume,
    pub input: Value<'gc>,
    pub waiting: Waiting<'gc>,
    pub settlement: Settlement<'gc>,
    pub promise: Option<JsObject<'gc>>,
    pub requests: Option<alloc::collections::VecDeque<Request<'gc>>>,
}

impl<'gc> Coroutine<'gc> {
    pub fn new(closure: Value<'gc>, promise: Option<JsObject<'gc>>, async_generator: bool) -> Self {
        Self {
            state: CoroutineState::Running,
            frame: Suspended { closure, params: Vec::new(), regs: Vec::new(), context: None, continuation: 0 },
            mode: crate::Resume::Next,
            input: Value::Undefined,
            waiting: Waiting::Nothing,
            settlement: Settlement::Nothing,
            promise,
            requests: async_generator.then(alloc::collections::VecDeque::new),
        }
    }
}

#[derive(Clone, Copy, Debug, Collect)]
#[collect(no_drop)]
pub enum Waiting<'gc> {
    Nothing,
    Await(Value<'gc>),
    Yield(Value<'gc>),
    Return,
    Rethrow,
}

#[derive(Clone, Copy, Debug, Collect)]
#[collect(no_drop)]
pub enum Settlement<'gc> {
    Nothing,
    Resolved(Value<'gc>),
    Rejected(Value<'gc>),
}

#[derive(Clone, Copy, Debug, Collect)]
#[collect(no_drop)]
pub struct Request<'gc> {
    pub mode: crate::Resume,
    pub value: Value<'gc>,
    pub promise: JsObject<'gc>,
}

#[derive(Clone, Copy, Debug, Collect)]
#[collect(no_drop)]
pub enum Settled<'gc> {
    Pending,
    Fulfilled(Value<'gc>),
    Rejected(Value<'gc>),
}

#[derive(Clone, Copy, Debug, Collect)]
#[collect(no_drop)]
pub enum Reaction<'gc> {
    Then { on_fulfilled: Value<'gc>, on_rejected: Value<'gc>, capability: Option<Capability<'gc>> },
    Await { coroutine: JsObject<'gc> },
    Host { hook: HostHook<'gc> },
}

pub struct HostReaction(pub Box<HostReactionFn>);

pub type HostReactionFn = dyn for<'g, 'x> FnOnce(&mut crate::Agent<'g, 'x>, Value<'g>, bool) + Send + Sync;

impl HostReaction {
    pub fn new(f: impl for<'g, 'x> FnOnce(&mut crate::Agent<'g, 'x>, Value<'g>, bool) + Send + Sync + 'static) -> Self {
        Self(Box::new(f))
    }
}

impl core::fmt::Debug for HostReaction {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        f.write_str("HostReaction")
    }
}

pub type HostHook<'gc> = Gc<'gc, crate::Static<j8lockfree::AtomicRefCell<Option<HostReaction>>>>;

#[derive(Clone, Copy, Debug, Collect)]
#[collect(no_drop)]
pub struct Capability<'gc> {
    pub promise: Value<'gc>,
    pub resolve: Value<'gc>,
    pub reject: Value<'gc>,
}

#[derive(Clone, Copy, Debug, Collect)]
#[collect(no_drop)]
pub enum Job<'gc> {
    Reaction { reaction: Reaction<'gc>, value: Value<'gc>, rejected: bool },
    Thenable { promise: JsObject<'gc>, thenable: Value<'gc>, then: Value<'gc> },
}

#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum MapKey {
    Undefined,
    Null,
    Bool(bool),
    Number(u64),
    String(SmallStr),
    BigInt(Box<str>),
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
            ValueKind::String(s) => MapKey::String(crate::SmallStr::from(s.as_str())),
            ValueKind::BigInt(b) => MapKey::BigInt(alloc::format!("{}", b.0).into()),
            ValueKind::Symbol(s) => MapKey::Ref(Gc::as_ptr(s) as usize),
            ValueKind::Object(o) => MapKey::Ref(Gc::as_ptr(o).cast::<u8>() as usize),
        }
    }
}

#[derive(Debug)]
pub struct Collection<'gc> {
    entries: Vec<Option<(Value<'gc>, Value<'gc>)>>,
    index: crate::Static<hashbrown::HashMap<MapKey, usize, core::hash::BuildHasherDefault<rustc_hash::FxHasher>>>,
    pub size: usize,
    pub weak: bool,
}

unsafe impl crate::Trace for Collection<'_> {
    fn trace(&self, t: &mut crate::Tracer<'_>) {
        self.entries.trace(t);
    }

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

    pub fn set(&mut self, key: Value<'gc>, value: Value<'gc>) {
        let key = match key.kind() {
            ValueKind::Number(x) => Value::Number(x + 0.0),
            k => k.into(),
        };
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

    pub fn clear(&mut self) {
        self.entries.iter_mut().for_each(|e| *e = None);
        self.index.0.clear();
        self.size = 0;
    }
}

pub type ClosureFn = for<'gc, 'x> fn(&mut Agent<'gc, 'x>, JsObject<'gc>, Value<'gc>, &[Value<'gc>]) -> Result<Value<'gc>, Error>;

#[derive(Collect)]
#[collect(no_drop)]
pub struct NativeClosure<'gc> {
    #[collect(require_static)]
    pub f: ClosureFn,
    pub slots: Vec<Value<'gc>>,
    #[collect(require_static)]
    pub constructor: bool,
}

impl fmt::Debug for NativeClosure<'_> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("NativeClosure").field("slots", &self.slots.len()).finish()
    }
}

#[derive(Clone, Copy, Debug, Collect)]
#[collect(no_drop)]
pub struct ProxyData<'gc> {
    pub target: Option<JsObject<'gc>>,
    pub handler: Option<JsObject<'gc>>,
    pub callable: bool,
    pub constructor: bool,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Collect)]
#[collect(require_static)]
pub enum HelperKind {
    Map,
    Filter,
    Take,
    Drop,
    FlatMap,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Collect)]
#[collect(require_static)]
pub enum HelperState {
    Start,
    Suspended,
    Running,
    Done,
}

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

    pub fn size(self) -> usize {
        match self {
            ElementKind::Int8 | ElementKind::Uint8 | ElementKind::Uint8Clamped => 1,
            ElementKind::Int16 | ElementKind::Uint16 | ElementKind::Float16 => 2,
            ElementKind::Int32 | ElementKind::Uint32 | ElementKind::Float32 => 4,
            ElementKind::Float64 | ElementKind::BigInt64 | ElementKind::BigUint64 => 8,
        }
    }

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

#[derive(Debug, Collect)]
#[collect(require_static)]
pub struct BufferData {
    pub bytes: Option<Backing>,
    pub max: Option<usize>,
    pub shared: bool,
}

#[derive(Clone, Copy, Debug, Collect)]
#[collect(no_drop)]
pub struct ViewData<'gc> {
    pub buffer: JsObject<'gc>,
    pub offset: usize,
    pub length: Option<usize>,
}

#[derive(Clone, Debug, Collect)]
#[collect(no_drop)]
pub struct ArgumentsMap<'gc> {
    pub env: JsEnv<'gc>,
    pub slots: Vec<Option<u16>>,
}

pub type HostData<'gc> = Gc<'gc, crate::Static<j8lockfree::AtomicRefCell<Box<dyn core::any::Any + Send + Sync>>>>;

#[derive(Debug, Default, Collect)]
#[collect(no_drop)]
pub enum ObjectKind<'gc> {
    #[default]
    Ordinary,
    Arguments(Option<ArgumentsMap<'gc>>),
    Error,
    Array,
    Function(Closure<'gc>),
    Native(Native),
    Bound(Box<Bound<'gc>>),
    Iterator(IteratorState<'gc>),
    Coroutine(Box<Coroutine<'gc>>),
    Promise { state: Settled<'gc>, reactions: Vec<Reaction<'gc>> },
    Resolver { promise: JsObject<'gc>, reject: bool, done: Gc<'gc, core::sync::atomic::AtomicBool> },
    RegExp(RegExpData<'gc>),
    Primitive(Value<'gc>),
    Map(Box<Collection<'gc>>),
    Set(Box<Collection<'gc>>),
    CollectionIterator { target: Option<JsObject<'gc>>, index: usize, kind: IterKind, set: bool },
    NativeClosure(NativeClosure<'gc>),
    Proxy(ProxyData<'gc>),
    IteratorHelper(Box<IteratorHelper<'gc>>),
    WrappedIterator(crate::IterRecord<'gc>),
    AccessorPair { get: Option<JsObject<'gc>>, set: Option<JsObject<'gc>> },
    AsyncFromSyncIterator(crate::IterRecord<'gc>),
    ArrayBuffer(Box<BufferData>),
    Date(f64),
    Host(HostData<'gc>),
    ModuleNamespace(Vec<(JsString<'gc>, crate::ResolvedBinding<'gc>)>),
    WeakRef(Value<'gc>),
    FinalizationRegistry { cleanup: Value<'gc>, cells: Vec<(Value<'gc>, Value<'gc>, Value<'gc>)> },
    RegExpStringIterator { matcher: JsObject<'gc>, string: JsString<'gc>, global: bool, unicode: bool, done: bool },
    TypedArray(ElementKind, ViewData<'gc>),
    DataView(ViewData<'gc>),
}

impl ObjectKind<'_> {
    pub fn has_ordinary_properties(&self) -> bool {
        !(matches!(self, ObjectKind::Proxy(_) | ObjectKind::Array | ObjectKind::TypedArray(..) | ObjectKind::ModuleNamespace(_) | ObjectKind::Arguments(_))
            || matches!(self, ObjectKind::Primitive(v) if matches!(v.kind(), ValueKind::String(_))))
    }

    pub fn is_callable(&self) -> bool {
        matches!(self, ObjectKind::Function(_) | ObjectKind::Native(_) | ObjectKind::Bound(_) | ObjectKind::Resolver { .. } | ObjectKind::NativeClosure(_) | ObjectKind::Proxy(ProxyData { callable: true, .. }))
    }
}

pub use crate::text::{JsStr, Pieces, SmallStr, Units};

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

#[derive(Default)]
pub struct ObjectData<'gc> {
    pub kind: ObjectKind<'gc>,
    pub capacity: usize,
    pub elements: Vec<Value<'gc>>,
    pub symbols: Few<(JsSymbol<'gc>, Property<'gc>)>,
    pub privates: Few<(JsSymbol<'gc>, Property<'gc>)>,
    pub proto: Option<JsObject<'gc>>,
    pub sparse_length: u32,
    pub non_extensible: bool,
    pub length_readonly: bool,
}

impl<'gc> ObjectData<'gc> {
    pub fn ordinary(mc: &Mutation<'gc>, proto: Option<JsObject<'gc>>, capacity: usize) -> JsObject<'gc> {
        mc.alloc(Object {
            proto: GcSlot::from(proto),
            call: GcSlot::default(),
            props: crate::Props::with_capacity(mc, capacity),
            elements: crate::ValueArray::default(),
            class: crate::IsoCell::new(Class::ORDINARY | Class::PLAIN | Class::UNIT_ORDINARY),
            non_extensible: crate::IsoCell::new(false),
            length_readonly: crate::IsoCell::new(false),
            digit_keys: crate::IsoCell::new(false),
            sparse_length: crate::IsoCell::new(0),
            shape_root: paragc::HandleSlot::default(),
            data: GcCell::empty(),
        })
    }

    pub fn alloc(self, mc: &Mutation<'gc>) -> JsObject<'gc> {
        let Self { kind, capacity, elements, symbols, privates, proto, sparse_length, non_extensible, length_readonly } = self;
        let state = ObjectState { kind, symbols, privates };
        let unit = Class::unit(&state);
        let class = Class::of(&state.kind) | unit.unwrap_or(0) | if state.symbols.is_empty() { 0 } else { Class::SYMBOLS };
        let call = Object::call_of(mc, &state.kind);
        let o = mc.alloc(Object {
            proto: GcSlot::from(proto),
            call: GcSlot::from(call),
            props: crate::Props::with_capacity(mc, capacity),
            elements: crate::ValueArray::default(),
            class: crate::IsoCell::new(class),
            non_extensible: crate::IsoCell::new(non_extensible),
            length_readonly: crate::IsoCell::new(length_readonly),
            digit_keys: crate::IsoCell::new(false),
            sparse_length: crate::IsoCell::new(sparse_length),
            shape_root: paragc::HandleSlot::default(),
            data: if unit.is_some() { GcCell::empty() } else { GcCell::new(mc, state) },
        });
        if !elements.is_empty() {
            o.elements.replace(mc, &elements);
        }
        o
    }
}

#[derive(Default, Collect)]
pub struct ObjectState<'gc> {
    pub kind: ObjectKind<'gc>,
    pub symbols: Few<(JsSymbol<'gc>, Property<'gc>)>,
    pub privates: Few<(JsSymbol<'gc>, Property<'gc>)>,
}

impl<'gc> ObjectState<'gc> {
    pub fn get_own_symbol(&self, key: JsSymbol<'gc>) -> Option<Property<'gc>> {
        self.symbols.iter().find_map(|&(k, p)| Gc::ptr_eq(k, key).then_some(p))
    }

    pub fn define_symbol(&mut self, key: JsSymbol<'gc>, prop: Property<'gc>) {
        match self.symbols.iter_mut().find(|(k, _)| Gc::ptr_eq(*k, key)) {
            Some((_, p)) => *p = prop,
            None => self.symbols.push((key, prop)),
        }
    }

}

impl fmt::Debug for ObjectState<'_> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let kind = match self.kind {
            ObjectKind::Ordinary | ObjectKind::Arguments(_) | ObjectKind::AccessorPair { .. } => "Object",
            ObjectKind::Error => "Error",
            ObjectKind::Array => "Array",
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
            ObjectKind::Iterator(_) | ObjectKind::CollectionIterator { .. } | ObjectKind::IteratorHelper(_) | ObjectKind::WrappedIterator(_) | ObjectKind::AsyncFromSyncIterator(_) => "Iterator",
            ObjectKind::Coroutine(_) => "Generator",
            ObjectKind::Promise { .. } => "Promise",
            ObjectKind::RegExp(_) => "RegExp",
            ObjectKind::Primitive(_) => "Primitive",
            ObjectKind::Map(_) => "Map",
            ObjectKind::Set(_) => "Set",
        };
        f.write_str(kind)
    }
}

#[derive(Collect)]
#[repr(C, u8)]
enum Fast<'gc> {
    Closure(Closure<'gc>),
    Native(Native),
    RegExp(RegExpData<'gc>),
}

struct Class;

impl Class {
    const ARRAY: u8 = 1;
    const ORDINARY: u8 = 1 << 1;
    const PLAIN: u8 = 1 << 2;
    const CALLABLE: u8 = 1 << 3;
    const SYMBOLS: u8 = 1 << 6;
    const WIDE: u8 = 1 << 7;
    const UNIT_SHIFT: u32 = 4;
    const UNIT_MASK: u8 = 0b11 << Self::UNIT_SHIFT;
    const UNIT_ORDINARY: u8 = 1 << Self::UNIT_SHIFT;
    const UNIT_ERROR: u8 = 2 << Self::UNIT_SHIFT;
    const UNIT_DERIVED: u8 = 3 << Self::UNIT_SHIFT;

    fn unit(state: &ObjectState<'_>) -> Option<u8> {
        if !state.symbols.is_empty() || !state.privates.is_empty() {
            return None;
        }
        match state.kind {
            ObjectKind::Ordinary => Some(Self::UNIT_ORDINARY),
            ObjectKind::Error => Some(Self::UNIT_ERROR),
            ObjectKind::Array | ObjectKind::Function(_) | ObjectKind::Native(_) | ObjectKind::RegExp(_) => Some(Self::UNIT_DERIVED),
            _ => None,
        }
    }

    fn unit_state<'gc>(class: u8, fast: Option<&Fast<'gc>>) -> ObjectState<'gc> {
        let kind = match (class & Self::UNIT_MASK, fast) {
            (Self::UNIT_ERROR, _) => ObjectKind::Error,
            (Self::UNIT_DERIVED, Some(Fast::Closure(c))) => ObjectKind::Function(*c),
            (Self::UNIT_DERIVED, Some(Fast::Native(n))) => ObjectKind::Native(*n),
            (Self::UNIT_DERIVED, Some(Fast::RegExp(r))) => ObjectKind::RegExp(*r),
            (Self::UNIT_DERIVED, None) => ObjectKind::Array,
            _ => ObjectKind::Ordinary,
        };
        ObjectState { kind, ..ObjectState::default() }
    }

    fn of(kind: &ObjectKind<'_>) -> u8 {
        let bit = |b: bool, v: u8| if b { v } else { 0 };
        bit(matches!(kind, ObjectKind::Array), Self::ARRAY)
            | bit(kind.has_ordinary_properties(), Self::ORDINARY)
            | bit(!matches!(kind, ObjectKind::Proxy(_) | ObjectKind::TypedArray(..) | ObjectKind::ModuleNamespace(_)), Self::PLAIN)
            | bit(kind.is_callable(), Self::CALLABLE)
    }
}

#[crate::heap]
pub struct Object<'gc> {
    #[mutable(set = write_proto)]
    proto: Option<JsObject<'gc>>,
    call: GcSlot<'gc, Fast<'gc>>,
    props: crate::Props<'gc>,
    elements: crate::ValueArray<'gc>,
    class: crate::IsoCell<u8>,
    #[mutable]
    non_extensible: bool,
    #[mutable]
    length_readonly: bool,
    #[mutable]
    digit_keys: bool,
    #[mutable]
    sparse_length: u32,
    shape_root: paragc::HandleSlot<'gc, crate::Shape>,
    data: GcCell<'gc, ObjectState<'gc>>,
}

const _: () = assert!(!core::mem::needs_drop::<Object<'static>>(), "objects need no destructor: sweeping skips them");

impl fmt::Debug for Object<'_> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_list().entries(self.props.entries().into_iter().map(|(k, _)| k)).finish()?;
        fmt::Debug::fmt(&self.data, f)
    }
}

pub struct ObjRef<'a, 'gc> {
    obj: &'a Object<'gc>,
    data: core::cell::OnceCell<StateRef<'a, 'gc>>,
}

enum StateRef<'a, 'gc> {
    Cell(CellRef<'a, ObjectState<'gc>>),
    Unit(core::mem::ManuallyDrop<ObjectState<'gc>>),
}

impl<'gc> Deref for ObjRef<'_, 'gc> {
    type Target = ObjectState<'gc>;

    fn deref(&self) -> &ObjectState<'gc> {
        match self.data.get_or_init(|| self.obj.state_ref()) {
            StateRef::Cell(c) => c,
            StateRef::Unit(u) => u,
        }
    }
}

pub struct ObjMut<'a, 'gc> {
    obj: &'a Object<'gc>,
    mc: &'a Mutation<'gc>,
    data: core::cell::OnceCell<CellMut<'a, ObjectState<'gc>>>,
}

impl<'gc> ObjMut<'_, 'gc> {
    fn cell(&self) -> &CellMut<'_, ObjectState<'gc>> {
        self.data.get_or_init(|| {
            let (class, fast) = (self.obj.class(), self.obj.fast());
            self.obj.data.try_borrow_mut_or(self.mc, || Class::unit_state(class, fast)).expect("object state already borrowed")
        })
    }
}

impl<'gc> Deref for ObjMut<'_, 'gc> {
    type Target = ObjectState<'gc>;

    fn deref(&self) -> &ObjectState<'gc> {
        self.cell()
    }
}

impl<'gc> DerefMut for ObjMut<'_, 'gc> {
    fn deref_mut(&mut self) -> &mut ObjectState<'gc> {
        self.cell();
        self.data.get_mut().expect("just borrowed")
    }
}

impl Drop for ObjMut<'_, '_> {
    fn drop(&mut self) {
        if let Some(d) = self.data.take() {
            let wide = self.obj.class() & Class::WIDE;
            self.obj.class.set(Class::of(&d.kind) | wide | if d.symbols.is_empty() { 0 } else { Class::SYMBOLS });
            let call = Object::call_of(self.mc, &d.kind);
            drop(d);
            if call.is_some() || self.obj.call.get().is_some() {
                self.obj.call.set(self.mc.mutator(), call);
            }
        }
    }
}

macro_rules! forward_reads {
    ($($guard:ident)*) => {$(
        impl<'a, 'gc> $guard<'a, 'gc> {
            pub fn object(&self) -> &'a Object<'gc> {
                self.obj
            }
            pub fn proto(&self) -> Option<JsObject<'gc>> {
                self.obj.proto()
            }
            pub fn props(&self) -> &'a crate::Props<'gc> {
                &self.obj.props
            }
            pub fn elements(&self) -> &'a crate::ValueArray<'gc> {
                &self.obj.elements
            }
            pub fn non_extensible(&self) -> bool {
                self.obj.non_extensible()
            }
            pub fn length_readonly(&self) -> bool {
                self.obj.length_readonly()
            }
            pub fn digit_keys(&self) -> bool {
                self.obj.digit_keys()
            }
            pub fn sparse_length(&self) -> u32 {
                self.obj.sparse_length()
            }
            pub fn array_length(&self) -> Option<u32> {
                self.obj.array_length()
            }
            pub fn sparse_elements(&self) -> Vec<(u32, Value<'gc>)> {
                self.obj.sparse_elements()
            }
            pub fn prop_at(&self, mc: &Mutation<'gc>, at: usize, key: &str) -> Option<Property<'gc>> {
                self.obj.prop_at(mc, at, key)
            }
            pub fn position(&self, key: &str) -> Option<usize> {
                self.obj.position(key)
            }
            pub fn get_own(&self, mc: &Mutation<'gc>, key: &str) -> Option<Property<'gc>> {
                self.obj.get_own(mc, key)
            }
            pub fn own(&self, mc: &Mutation<'gc>, key: &str) -> Option<Value<'gc>> {
                self.obj.own(mc, key)
            }
            pub fn root_shape(&self, mc: &Mutation<'gc>) -> crate::ShapeRef<'gc> {
                self.obj.root_shape(mc)
            }
        }
    )*};
}
forward_reads!(ObjRef ObjMut);

impl<'gc> ObjRef<'_, 'gc> {
    pub fn own_slot(&self, mc: &Mutation<'gc>, key: Key<'_, 'gc>) -> Option<Property<'gc>> {
        match key {
            Key::Str(k) => self.obj.own_str(mc, k),
            Key::Sym(s) => self.get_own_symbol(s),
        }
    }
}

impl<'gc> ObjMut<'_, 'gc> {
    pub fn own_slot(&mut self, key: Key<'_, 'gc>) -> Option<Property<'gc>> {
        match key {
            Key::Str(k) => self.obj.own_str(self.mc, k),
            Key::Sym(s) => self.deref_mut().get_own_symbol(s),
        }
    }

    pub fn set_proto(&self, proto: Option<JsObject<'gc>>) {
        self.obj.set_proto(self.mc, proto);
    }

    pub fn set_non_extensible(&self, v: bool) {
        self.obj.non_extensible.set(v);
    }

    pub fn set_length_readonly(&self, v: bool) {
        self.obj.length_readonly.set(v);
    }

    pub fn write_index(&self, i: u32, value: Value<'gc>) -> bool {
        self.obj.write_index(self.mc, i, value)
    }

    pub fn fill_index(&self, from: u32, to: u32, value: Value<'gc>) -> bool {
        self.obj.fill_index(self.mc, from, to, value)
    }

    pub fn spill(&self, from: usize) {
        self.obj.spill(self.mc, from);
    }

    pub fn set_array_length(&self, n: u32) {
        self.obj.set_array_length(self.mc, n);
    }

    pub fn set_at(&self, at: usize, value: Value<'gc>) {
        self.obj.set_at(self.mc, at, value);
    }

    pub fn push_to(&self, to: crate::ShapeRef<'gc>, slot: Slot<'gc>) {
        self.obj.push_to(self.mc, to, slot);
    }

    pub fn set(&self, key: &str, value: Value<'gc>) {
        self.obj.set(self.mc, key, value);
    }

    pub fn define(&self, key: &str, prop: Property<'gc>) {
        self.obj.define(self.mc, key, prop);
    }

    pub fn remove(&self, key: &str) -> bool {
        self.obj.remove(self.mc, key)
    }

    pub fn store(&mut self, key: Key<'_, 'gc>, prop: Property<'gc>) {
        match key {
            Key::Sym(s) => self.deref_mut().define_symbol(s, prop),
            Key::Str(k) => self.obj.store_str(self.mc, k, prop),
        }
    }

    pub fn unstore(&mut self, key: Key<'_, 'gc>) {
        match key {
            Key::Sym(s) => self.deref_mut().symbols.retain(|(k, _)| !Gc::ptr_eq(*k, s)),
            Key::Str(k) => self.obj.unstore_str(self.mc, k),
        }
    }

    pub fn ordinary_put(&mut self, key: Key<'_, 'gc>, value: Value<'gc>) -> Option<bool> {
        if !self.obj.has_ordinary_properties() {
            return None;
        }
        match key {
            Key::Str(k) => self.obj.put_str(self.mc, k, value),
            Key::Sym(s) => {
                let non_extensible = self.obj.non_extensible();
                let d: &mut ObjectState<'gc> = self;
                match d.symbols.iter_mut().find(|(k, _)| Gc::ptr_eq(*k, s)).map(|(_, p)| (p.writable, &mut p.slot)) {
                    Some((true, Slot::Data(v))) => {
                        *v = value;
                        Some(true)
                    }
                    Some((false, Slot::Data(_))) => Some(false),
                    Some(_) => None,
                    None if non_extensible => Some(false),
                    None => {
                        d.define_symbol(s, Property::data(value));
                        Some(true)
                    }
                }
            }
        }
    }
}

impl<'gc> Object<'gc> {
    pub const PROTO_OFFSET: usize = core::mem::offset_of!(Object<'static>, proto);
    pub const CALL_OFFSET: usize = core::mem::offset_of!(Object<'static>, call);
    pub const PROPS_OFFSET: usize = core::mem::offset_of!(Object<'static>, props);
    pub const CLASS_OFFSET: usize = core::mem::offset_of!(Object<'static>, class);
    pub const PLAIN_BIT: u8 = Class::PLAIN;

    pub fn borrow(&self) -> ObjRef<'_, 'gc> {
        ObjRef { obj: self, data: core::cell::OnceCell::new() }
    }

    pub fn try_borrow(&self) -> Option<ObjRef<'_, 'gc>> {
        let r = self.borrow();
        let d = if self.data.has_value() { StateRef::Cell(self.data.try_borrow()?) } else { StateRef::Unit(core::mem::ManuallyDrop::new(Class::unit_state(self.class(), self.fast()))) };
        let _ = r.data.set(d);
        Some(r)
    }

    fn state_ref(&self) -> StateRef<'_, 'gc> {
        if self.data.has_value() { StateRef::Cell(self.data.borrow()) } else { StateRef::Unit(core::mem::ManuallyDrop::new(Class::unit_state(self.class(), self.fast()))) }
    }

    pub fn borrow_mut<'a>(&'a self, mc: &'a Mutation<'gc>) -> ObjMut<'a, 'gc> {
        ObjMut { obj: self, mc, data: core::cell::OnceCell::new() }
    }

    fn call_of(mc: &Mutation<'gc>, kind: &ObjectKind<'gc>) -> Option<Gc<'gc, Fast<'gc>>> {
        match kind {
            ObjectKind::Function(c) => Some(mc.alloc(Fast::Closure(*c))),
            ObjectKind::Native(n) => Some(mc.alloc(Fast::Native(*n))),
            ObjectKind::RegExp(r) => Some(mc.alloc(Fast::RegExp(*r))),
            _ => None,
        }
    }

    fn fast(&self) -> Option<&'gc Fast<'gc>> {
        self.call.get().map(Gc::get)
    }

    #[inline]
    pub fn native(&self) -> Option<Native> {
        match self.fast()? {
            Fast::Native(n) => Some(*n),
            _ => None,
        }
    }

    #[inline]
    pub fn regexp(&self) -> Option<RegExpData<'gc>> {
        match self.fast()? {
            Fast::RegExp(r) => Some(*r),
            _ => None,
        }
    }

    #[inline]
    pub fn has_symbols(&self) -> bool {
        self.class() & Class::SYMBOLS != 0
    }

    #[inline]
    pub fn closure(&self) -> Option<Closure<'gc>> {
        match self.fast()? {
            Fast::Closure(c) => Some(*c),
            _ => None,
        }
    }

    pub fn set_proto(&self, mc: &Mutation<'gc>, proto: Option<JsObject<'gc>>) {
        self.write_proto(mc, proto);
    }

    #[inline]
    pub fn props(&self) -> &crate::Props<'gc> {
        &self.props
    }

    #[inline]
    pub fn elements(&self) -> &crate::ValueArray<'gc> {
        &self.elements
    }

    fn class(&self) -> u8 {
        self.class.get()
    }

    #[inline]
    pub fn is_array(&self) -> bool {
        self.class() & Class::ARRAY != 0
    }

    #[inline]
    pub fn has_ordinary_properties(&self) -> bool {
        self.class() & Class::ORDINARY != 0
    }

    #[inline]
    pub fn is_plain(&self) -> bool {
        self.class() & Class::PLAIN != 0
    }

    #[inline]
    pub fn is_callable(&self) -> bool {
        self.class() & Class::CALLABLE != 0
    }

    fn raise_sparse_length(&self, n: u32) {
        self.sparse_length.set(self.sparse_length.get().max(n));
    }

    pub fn array_length(&self) -> Option<u32> {
        self.is_array().then(|| u32::try_from(self.elements.len()).unwrap_or(u32::MAX).max(self.sparse_length()))
    }

    pub fn write_index(&self, mc: &Mutation<'gc>, i: u32, value: Value<'gc>) -> bool {
        if !self.is_array() {
            return false;
        }
        let (at, n) = (i as usize, self.elements.len());
        if at < n {
            self.elements.set(mc, at, value);
            return true;
        }
        if at == n && self.can_append(i) {
            self.elements.push(mc, value);
            return true;
        }
        false
    }

    pub fn fill_index(&self, mc: &Mutation<'gc>, from: u32, to: u32, value: Value<'gc>) -> bool {
        let n = self.elements.len();
        if !self.is_array() || from as usize > n || to == u32::MAX {
            return false;
        }
        if to as usize > n && !self.can_append(u32::try_from(n).unwrap_or(u32::MAX)) {
            return false;
        }
        self.elements.fill(mc, from as usize, to as usize, value);
        true
    }

    fn can_append(&self, i: u32) -> bool {
        !self.non_extensible() && !self.length_readonly() && (self.sparse_length() <= i || !self.digit_keys()) && self.no_indexed_protos()
    }

    pub fn no_indexed_protos(&self) -> bool {
        let mut cur = self.proto();
        while let Some(p) = cur {
            let indexed = if p.is_array() {
                !p.elements.is_empty()
            } else if p.has_ordinary_properties() {
                false
            } else {
                !matches!(p.borrow().kind, ObjectKind::Arguments(_))
            };
            if indexed || p.sparse_length() > 0 || p.digit_keys() {
                return false;
            }
            cur = p.proto();
        }
        true
    }

    pub fn spill(&self, mc: &Mutation<'gc>, from: usize) {
        let n = self.elements.len();
        if !self.is_array() || from >= n {
            return;
        }
        let moved: Vec<Value<'gc>> = (from..n).map(|i| self.elements.get(i).unwrap_or_default()).collect();
        self.elements.truncate(mc, from);
        for (i, v) in (from..).zip(moved) {
            self.push_prop(mc, &alloc::format!("{i}"), Property::data(v));
        }
        self.raise_sparse_length(u32::try_from(n).unwrap_or(u32::MAX));
    }

    pub fn sparse_elements(&self) -> Vec<(u32, Value<'gc>)> {
        let dense = self.elements.len();
        if !self.is_array() || self.sparse_length() as usize <= dense {
            return Vec::new();
        }
        let mut out: Vec<(u32, Value<'gc>)> = self
            .props
            .entries()
            .into_iter()
            .filter_map(|(k, p)| match (crate::ops::index_of_str(&k), p.slot) {
                (Some(i), Slot::Data(v)) if i as usize >= dense => Some((i, v)),
                _ => None,
            })
            .collect();
        out.sort_unstable_by_key(|&(i, _)| i);
        out
    }

    pub fn set_array_length(&self, mc: &Mutation<'gc>, n: u32) {
        if !self.is_array() {
            return;
        }
        let len = n as usize;
        self.elements.truncate(mc, len);
        let dense = self.elements.len();
        if self.sparse_length() > 0 {
            self.props.retain(mc, |k| crate::ops::index_of_str(k).is_none_or(|i| i < n));
        }
        self.sparse_length.set(if len > dense { n } else { 0 });
    }

    pub fn own_str(&self, mc: &Mutation<'gc>, k: &str) -> Option<Property<'gc>> {
        if self.is_array()
            && let Some(i) = crate::ops::index_of_str(k)
            && let Some(v) = self.elements.get(i as usize)
        {
            return Some(Property::data(v));
        }
        self.get_own(mc, k)
    }

    pub fn own_slot(&self, mc: &Mutation<'gc>, key: Key<'_, 'gc>) -> Option<Property<'gc>> {
        match key {
            Key::Str(k) => self.own_str(mc, k),
            Key::Sym(_) if !self.has_symbols() => None,
            Key::Sym(s) => self.borrow().get_own_symbol(s),
        }
    }

    fn put_str(&self, mc: &Mutation<'gc>, k: &str, value: Value<'gc>) -> Option<bool> {
        match self.props.position(k) {
            Some(i) => match (self.props.attrs(i).writable(), self.props.slot(i)) {
                (true, Slot::Data(_)) => {
                    self.props.set_slot(mc, i, Slot::Data(value));
                    Some(true)
                }
                (false, Slot::Data(_)) => Some(false),
                _ => None,
            },
            None if self.non_extensible() => Some(false),
            None => {
                self.store_str(mc, k, Property::data(value));
                Some(true)
            }
        }
    }

    pub fn store_str(&self, mc: &Mutation<'gc>, k: &str, prop: Property<'gc>) {
        let plain = prop.enumerable && prop.writable && prop.configurable;
        let index = if self.is_array() { crate::ops::index_of_str(k) } else { None };
        if let Some(i) = index {
            let (at, n) = (i as usize, self.elements.len());
            let stored = self.get_own(mc, k).is_some();
            match (prop.slot, plain) {
                (Slot::Data(v), true) if at < n => return self.elements.set(mc, at, v),
                (Slot::Data(v), true) if at == n && !stored => return self.elements.push(mc, v),
                _ if at < n => self.spill(mc, at),
                _ => {}
            }
        }
        self.define(mc, k, prop);
        if let Some(i) = index {
            self.raise_sparse_length(i.saturating_add(1));
        }
    }

    pub fn unstore_str(&self, mc: &Mutation<'gc>, k: &str) {
        if self.is_array()
            && let Some(i) = crate::ops::index_of_str(k)
            && (i as usize) < self.elements.len()
        {
            self.spill(mc, i as usize);
        }
        if let Some(i) = self.position(k) {
            self.props.remove(mc, i);
        }
    }

    pub fn prop_at(&self, mc: &Mutation<'gc>, at: usize, key: &str) -> Option<Property<'gc>> {
        let p = self.props.get_if(at, key)?;
        Some(self.resolved(mc, at, p))
    }

    #[inline]
    fn resolved(&self, mc: &Mutation<'gc>, i: usize, mut p: Property<'gc>) -> Property<'gc> {
        if let Slot::Data(v) = p.slot
            && let Some(l) = v.as_lazy()
        {
            let v = l.resolve(self, mc);
            self.props.set_slot(mc, i, Slot::Data(v));
            p.slot = Slot::Data(v);
        }
        p
    }

    pub fn set_at(&self, mc: &Mutation<'gc>, at: usize, value: Value<'gc>) {
        if at < self.props.len() {
            self.props.set_slot(mc, at, Slot::Data(value));
        }
    }

    pub fn position(&self, key: &str) -> Option<usize> {
        self.props.position(key)
    }

    fn push_prop(&self, mc: &Mutation<'gc>, key: &str, prop: Property<'gc>) {
        if key.as_bytes().first().is_some_and(u8::is_ascii_digit) {
            self.digit_keys.set(true);
        }
        let proto = self.proto();
        self.props.push(mc, key, prop, self.class() & Class::WIDE != 0, || proto.map(|p| p.get().root_shape(mc)));
    }

    pub fn widen(&self) {
        self.class.set(self.class.get() | Class::WIDE);
    }

    pub fn root_shape(&self, mc: &Mutation<'gc>) -> crate::ShapeRef<'gc> {
        match self.shape_root.get() {
            Some(h) => h,
            None => {
                let h = crate::Shape::empty(mc);
                self.shape_root.set(Some(h));
                h
            }
        }
    }

    pub fn push_to(&self, mc: &Mutation<'gc>, to: crate::ShapeRef<'gc>, slot: Slot<'gc>) {
        if to.digit_keys() {
            self.digit_keys.set(true);
        }
        self.props.push_to(mc, to, slot);
    }

    pub fn fill(&self, mc: &Mutation<'gc>, to: crate::ShapeRef<'gc>, slots: impl IntoIterator<Item = Slot<'gc>>) {
        if to.digit_keys() {
            self.digit_keys.set(true);
        }
        self.props.fill(mc, to, slots);
    }

    pub fn get_own(&self, mc: &Mutation<'gc>, key: &str) -> Option<Property<'gc>> {
        self.props.lookup(key).map(|(i, p)| self.resolved(mc, i, p))
    }

    pub fn own(&self, mc: &Mutation<'gc>, key: &str) -> Option<Value<'gc>> {
        self.get_own(mc, key).and_then(|p| p.value())
    }

    pub fn set(&self, mc: &Mutation<'gc>, key: &str, value: Value<'gc>) {
        match self.position(key) {
            Some(i) => {
                if self.props.attrs(i).writable() {
                    self.props.set_slot(mc, i, Slot::Data(value));
                }
            }
            None => self.push_prop(mc, key, Property::data(value)),
        }
    }

    pub fn define(&self, mc: &Mutation<'gc>, key: &str, prop: Property<'gc>) {
        match self.position(key) {
            Some(i) => self.props.replace(mc, i, prop),
            None => self.push_prop(mc, key, prop),
        }
    }

    pub fn remove(&self, mc: &Mutation<'gc>, key: &str) -> bool {
        match self.position(key) {
            Some(i) if self.props.attrs(i).configurable() => {
                self.props.remove(mc, i);
                true
            }
            Some(_) => false,
            None => true,
        }
    }
}

impl<'gc> From<ObjectKind<'gc>> for ObjectData<'gc> {
    fn from(kind: ObjectKind<'gc>) -> Self {
        Self { kind, ..Self::default() }
    }
}
