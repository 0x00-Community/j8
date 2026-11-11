use alloc::boxed::Box;
use alloc::vec::Vec;
use core::fmt;

use gc_arena::{Collect, Gc, Lock, Mutation, RefLock};

use crate::{Agent, Error, FuncId, Key, Value};

/// 堆上的字符串。
pub type JsString<'gc> = Gc<'gc, Box<str>>;

/// 一个 Symbol：只有描述，身份就是指针。
#[derive(Debug, Collect)]
#[collect(require_static)]
pub struct SymbolData {
    pub description: Option<Box<str>>,
}

/// 堆上的 Symbol。
pub type JsSymbol<'gc> = Gc<'gc, SymbolData>;

/// 一个 BigInt（任意精度整数）。
#[derive(Debug, Collect)]
#[collect(require_static)]
pub struct BigIntData(pub num_bigint::BigInt);

/// 堆上的 BigInt。
pub type JsBigInt<'gc> = Gc<'gc, BigIntData>;

/// 正则对象的本体：源码、flags，以及正则引擎编好的东西（类型擦除，由 r8builtins 解释）。
#[derive(Debug, Collect)]
#[collect(no_drop)]
pub struct RegExpData<'gc> {
    pub source: JsString<'gc>,
    pub flags: JsString<'gc>,
    pub compiled: Gc<'gc, gc_arena::Static<Box<dyn core::any::Any>>>,
    /// annexB 的旧特性（`compile`）能用：只有 %RegExp% 自己建的（子类实例不行）
    pub legacy: bool,
}

/// 堆上的对象（内部可变，写经 gc-arena 的写屏障）。
pub type JsObject<'gc> = Gc<'gc, RefLock<ObjectData<'gc>>>;

/// 堆上的作用域环境：被内层函数捕获的绑定放在这里，按槽位存。
pub type JsEnv<'gc> = Gc<'gc, RefLock<Env<'gc>>>;

/// 宿主函数：`this` 与实参进，值或异常出。
pub type NativeFn = for<'gc, 'x> fn(&mut Agent<'gc, 'x>, Value<'gc>, &[Value<'gc>]) -> Result<Value<'gc>, Error>;

/// 一层作用域环境，`parent` 指向外层。
#[derive(Debug, Collect)]
#[collect(no_drop)]
pub struct Env<'gc> {
    pub slots: Vec<Value<'gc>>,
    pub parent: Option<JsEnv<'gc>>,
}

impl<'gc> Env<'gc> {
    pub fn alloc(self, mc: &Mutation<'gc>) -> JsEnv<'gc> {
        Gc::new(mc, RefLock::new(self))
    }
}

/// 闭包：函数编号、创建时的环境、方法的主对象（`super` 从它的原型找）、类构造器的实例字段初始化函数。
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

/// 属性的内容：数据，或一对访问器。
#[derive(Clone, Copy, Debug, Collect)]
#[collect(no_drop)]
pub enum Slot<'gc> {
    Data(Value<'gc>),
    Accessor { get: Option<JsObject<'gc>>, set: Option<JsObject<'gc>> },
}

/// 一个属性：内容加特性。
#[derive(Clone, Copy, Debug, Collect)]
#[collect(no_drop)]
pub struct Property<'gc> {
    pub slot: Slot<'gc>,
    pub enumerable: bool,
    pub writable: bool,
    pub configurable: bool,
}

impl<'gc> Property<'gc> {
    /// 普通赋值建出来的属性：可枚举、可写、可配置。
    pub fn data(v: Value<'gc>) -> Self {
        Self { slot: Slot::Data(v), enumerable: true, writable: true, configurable: true }
    }

    /// 内建对象与类方法的属性：不可枚举。
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

/// 宿主函数对象的本体。
#[derive(Clone, Copy, Collect)]
#[collect(require_static)]
pub struct Native {
    pub f: NativeFn,
    /// 能 `new`（内建构造器）；能的时候 [`Agent::new_target`] 告诉它是不是构造调用
    pub constructor: bool,
}

impl Native {
    /// 普通宿主函数（不能 `new`）。
    pub fn function(f: NativeFn) -> Self {
        Self { f, constructor: false }
    }
}

impl fmt::Debug for Native {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("Native")
    }
}

/// `Function.prototype.bind` 的结果：调用时换成目标，`this` 与前置实参固定。
#[derive(Debug, Collect)]
#[collect(no_drop)]
pub struct Bound<'gc> {
    pub target: Value<'gc>,
    pub this: Value<'gc>,
    pub args: Vec<Value<'gc>>,
}

/// 数组迭代器给什么。
#[derive(Clone, Copy, Debug, PartialEq, Eq, Collect)]
#[collect(require_static)]
pub enum IterKind {
    Values,
    Keys,
    Entries,
}

/// 内建迭代器的状态。
#[derive(Debug, Collect)]
#[collect(no_drop)]
pub enum IteratorState<'gc> {
    /// 数组（或类数组）按下标走，`target` 为 `None` 就是走完了
    Array { target: Option<Value<'gc>>, index: usize, kind: IterKind },
    /// 字符串按码点走（`pos` 是 UTF-8 字节位置）
    String { s: JsString<'gc>, pos: usize },
}

/// 挂起的一帧：生成器 / async 函数在 `yield` / `await` 处存下的全部执行状态。
#[derive(Debug, Collect)]
#[collect(no_drop)]
pub struct Suspended<'gc> {
    pub func: FuncId,
    pub pc: usize,
    pub regs: Vec<Value<'gc>>,
    pub env: Option<JsEnv<'gc>>,
    /// 装着的异常处理器：（落点, 装时的环境）
    pub handlers: Vec<(u32, Option<JsEnv<'gc>>)>,
    pub args: Vec<Value<'gc>>,
    /// 恢复时写恢复方式的寄存器（还没开始跑时没有）
    pub mode: Option<u16>,
}

/// 协程（生成器 / async 函数）的状态。
#[derive(Debug, Collect)]
#[collect(no_drop)]
pub enum CoroutineState<'gc> {
    /// 还没开始或停在 `yield` / `await`
    Suspended(Suspended<'gc>),
    /// 正在跑（重入 `next` 抛 TypeError）
    Running,
    Completed,
}

/// 生成器或 async 函数的协程；async 函数带着它的结果 Promise。
#[derive(Debug, Collect)]
#[collect(no_drop)]
pub struct Coroutine<'gc> {
    pub state: CoroutineState<'gc>,
    /// 还没跑过第一步（这时 `return` / `throw` 直接结束）
    pub fresh: bool,
    pub promise: Option<JsObject<'gc>>,
    /// async 生成器：排着的 `next` / `throw` / `return` 请求，队头是正在处理的那个
    pub requests: Option<alloc::collections::VecDeque<Request<'gc>>>,
}

impl<'gc> Coroutine<'gc> {
    /// 一个还没开始的协程：生成器（`promise` 与 `requests` 都没有）、async 函数（带结果 Promise）、
    /// async 生成器（带请求队列）。
    pub fn new(saved: Suspended<'gc>, promise: Option<JsObject<'gc>>, async_generator: bool) -> Self {
        Self { state: CoroutineState::Suspended(saved), fresh: true, promise, requests: async_generator.then(alloc::collections::VecDeque::new) }
    }
}

/// async 生成器的一个请求：怎么恢复、送进去的值、给调用者的 Promise。
#[derive(Clone, Copy, Debug, Collect)]
#[collect(no_drop)]
pub struct Request<'gc> {
    pub mode: crate::Resume,
    pub value: Value<'gc>,
    pub promise: JsObject<'gc>,
}

/// Promise 的状态。
#[derive(Clone, Copy, Debug, Collect)]
#[collect(no_drop)]
pub enum Settled<'gc> {
    Pending,
    Fulfilled(Value<'gc>),
    Rejected(Value<'gc>),
}

/// Promise 落定后要做的事。
#[derive(Clone, Copy, Debug, Collect)]
#[collect(no_drop)]
pub enum Reaction<'gc> {
    /// `then(onFulfilled, onRejected)`，结果交给能力记录的 resolve / reject（没有就丢掉）
    Then { on_fulfilled: Value<'gc>, on_rejected: Value<'gc>, capability: Option<Capability<'gc>> },
    /// `await`：恢复这个协程
    Await { coroutine: JsObject<'gc> },
}

/// PromiseCapability：结果 Promise 与它的 resolve / reject 函数。
#[derive(Clone, Copy, Debug, Collect)]
#[collect(no_drop)]
pub struct Capability<'gc> {
    pub promise: Value<'gc>,
    pub resolve: Value<'gc>,
    pub reject: Value<'gc>,
}

/// 微任务。
#[derive(Clone, Copy, Debug, Collect)]
#[collect(no_drop)]
pub enum Job<'gc> {
    /// 一个反应：`value` 是落定的值，`rejected` 说是拒绝
    Reaction { reaction: Reaction<'gc>, value: Value<'gc>, rejected: bool },
    /// 用 thenable 解决 Promise：调 `then.call(thenable, resolve, reject)`
    Thenable { promise: JsObject<'gc>, thenable: Value<'gc>, then: Value<'gc> },
}

/// Map / Set 按 SameValueZero 找键用的排序键：`NaN` 只有一个、`-0` 当 `+0`，字符串与 BigInt 比内容，对象与 Symbol 比身份。
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub enum MapKey {
    Undefined,
    Null,
    Bool(bool),
    Number(u64),
    String(Box<str>),
    BigInt(Box<str>),
    /// 对象 / Symbol 的地址（gc-arena 不搬对象，地址就是身份）
    Ref(usize),
}

impl From<Value<'_>> for MapKey {
    fn from(v: Value<'_>) -> Self {
        match v {
            Value::Undefined | Value::Uninit => MapKey::Undefined,
            Value::Null => MapKey::Null,
            Value::Bool(b) => MapKey::Bool(b),
            Value::Number(x) if x.is_nan() => MapKey::Number(f64::NAN.to_bits()),
            Value::Number(x) => MapKey::Number((x + 0.0).to_bits()),
            Value::String(s) => MapKey::String((**s).into()),
            Value::BigInt(b) => MapKey::BigInt(alloc::format!("{}", b.0).into()),
            Value::Symbol(s) => MapKey::Ref(Gc::as_ptr(s) as usize),
            Value::Object(o) => MapKey::Ref(Gc::as_ptr(o).cast::<u8>() as usize),
        }
    }
}

/// Map / Set 的存储：按插入顺序的条目（删掉的留空位，迭代中删改也能接着走）加按键找下标的索引。
#[derive(Debug, Collect)]
#[collect(no_drop)]
pub struct Collection<'gc> {
    pub entries: Vec<Option<(Value<'gc>, Value<'gc>)>>,
    index: gc_arena::Static<alloc::collections::BTreeMap<MapKey, usize>>,
    pub size: usize,
    pub weak: bool,
}

impl<'gc> Collection<'gc> {
    pub fn new(weak: bool) -> Self {
        Self { entries: Vec::new(), index: gc_arena::Static(alloc::collections::BTreeMap::new()), size: 0, weak }
    }

    pub fn get(&self, key: Value<'gc>) -> Option<Value<'gc>> {
        let i = *self.index.0.get(&MapKey::from(key))?;
        self.entries[i].map(|(_, v)| v)
    }

    pub fn has(&self, key: Value<'gc>) -> bool {
        self.index.0.contains_key(&MapKey::from(key))
    }

    /// 设值：已有就改，否则追加到末尾。`-0` 键存成 `+0`。
    pub fn set(&mut self, key: Value<'gc>, value: Value<'gc>) {
        // `-0 + 0` 是 `+0`
        let key = match key {
            Value::Number(x) => Value::Number(x + 0.0),
            k => k,
        };
        match self.index.0.get(&MapKey::from(key)) {
            Some(&i) => self.entries[i] = Some((key, value)),
            None => {
                self.index.0.insert(MapKey::from(key), self.entries.len());
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

    /// 清空：条目都留成空位，正在走的迭代器走到末尾就停。
    pub fn clear(&mut self) {
        self.entries.iter_mut().for_each(|e| *e = None);
        self.index.0.clear();
        self.size = 0;
    }
}

/// 带状态的宿主函数：调用时拿到自己（状态在 `slots` 里）。
pub type ClosureFn = for<'gc, 'x> fn(&mut Agent<'gc, 'x>, JsObject<'gc>, Value<'gc>, &[Value<'gc>]) -> Result<Value<'gc>, Error>;

/// 带状态的宿主函数对象的本体（Promise 的元素函数、Proxy 的撤销函数等）。
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

/// Proxy：目标与处理器（撤销后都是 `None`）；能不能调用 / 构造在建的时候按目标定下。
#[derive(Clone, Copy, Debug, Collect)]
#[collect(no_drop)]
pub struct ProxyData<'gc> {
    pub target: Option<JsObject<'gc>>,
    pub handler: Option<JsObject<'gc>>,
    pub callable: bool,
    pub constructor: bool,
}

/// Iterator 助手的种类（`map` / `filter` / `take` / `drop` / `flatMap`）。
#[derive(Clone, Copy, Debug, PartialEq, Eq, Collect)]
#[collect(require_static)]
pub enum HelperKind {
    Map,
    Filter,
    Take,
    Drop,
    FlatMap,
}

/// Iterator 助手（生成器式）的状态。
#[derive(Clone, Copy, Debug, PartialEq, Eq, Collect)]
#[collect(require_static)]
pub enum HelperState {
    Start,
    Suspended,
    Running,
    Done,
}

/// Iterator 助手：底层迭代器、回调、`take` / `drop` 剩下的个数、计数、`flatMap` 正在走的内层迭代器。
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

/// TypedArray 的元素类型。
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

    /// 一个元素几个字节。
    pub fn size(self) -> usize {
        match self {
            ElementKind::Int8 | ElementKind::Uint8 | ElementKind::Uint8Clamped => 1,
            ElementKind::Int16 | ElementKind::Uint16 | ElementKind::Float16 => 2,
            ElementKind::Int32 | ElementKind::Uint32 | ElementKind::Float32 => 4,
            ElementKind::Float64 | ElementKind::BigInt64 | ElementKind::BigUint64 => 8,
        }
    }

    /// 构造器名。
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

    /// DataView 方法名里的类型（`getInt8` 的 `Int8`）。
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

/// ArrayBuffer / SharedArrayBuffer 的本体：字节（分离后没有）、可调大小的上限、是不是共享的。
#[derive(Debug, Collect)]
#[collect(require_static)]
pub struct BufferData {
    pub bytes: Option<Vec<u8>>,
    pub max: Option<usize>,
    pub shared: bool,
}

/// TypedArray / DataView 看的那段：所在的缓冲、起点、长度（`None` 是跟着缓冲变长短）。
#[derive(Clone, Copy, Debug, Collect)]
#[collect(no_drop)]
pub struct ViewData<'gc> {
    pub buffer: JsObject<'gc>,
    pub offset: usize,
    pub length: Option<usize>,
}

/// 对象的种类：普通对象之外带的内部槽。
#[derive(Debug, Default, Collect)]
#[collect(no_drop)]
pub enum ObjectKind<'gc> {
    #[default]
    Ordinary,
    /// 非映射的 arguments 对象（`Object.prototype.toString` 认它）
    Arguments,
    /// Error 实例（有 [[ErrorData]]）
    Error,
    /// 数组元素（下标属性存在这里，`length` 就是它的长度）
    Array(Vec<Value<'gc>>),
    Function(Closure<'gc>),
    Native(Native),
    Bound(Bound<'gc>),
    Iterator(IteratorState<'gc>),
    Coroutine(Coroutine<'gc>),
    Promise { state: Settled<'gc>, reactions: Vec<Reaction<'gc>> },
    /// Promise 的 resolve / reject 函数：共享一个「已经解决过」的标记
    Resolver { promise: JsObject<'gc>, reject: bool, done: Gc<'gc, Lock<bool>> },
    RegExp(RegExpData<'gc>),
    /// 原始值包装对象（`new Boolean(…)` / `new Number(…)` / `new String(…)` / `Object(sym)` …）：存着被包的原始值
    Primitive(Value<'gc>),
    /// Map（`weak` 为真是 WeakMap）
    Map(Collection<'gc>),
    /// Set（值放在键与值两处；`weak` 为真是 WeakSet）
    Set(Collection<'gc>),
    /// Map / Set 的迭代器：活视图，按条目下标走（`target` 为 `None` 就是走完了）
    CollectionIterator { target: Option<JsObject<'gc>>, index: usize, kind: IterKind, set: bool },
    NativeClosure(NativeClosure<'gc>),
    Proxy(ProxyData<'gc>),
    IteratorHelper(IteratorHelper<'gc>),
    /// `Iterator.from` 包出来的迭代器
    WrappedIterator(crate::IterRecord<'gc>),
    ArrayBuffer(BufferData),
    /// Date：时间值（毫秒，无效是 NaN）
    Date(f64),
    /// WeakRef（目前强引用目标）
    WeakRef(Value<'gc>),
    /// FinalizationRegistry：清理回调与登记的（目标, 携带值, 注销令牌）（目前强引用，不会触发清理）
    FinalizationRegistry { cleanup: Value<'gc>, cells: Vec<(Value<'gc>, Value<'gc>, Value<'gc>)> },
    /// `matchAll` 的迭代器：匹配器、字符串、带不带 `g`、按码点走、走完没有
    RegExpStringIterator { matcher: JsObject<'gc>, string: JsString<'gc>, global: bool, unicode: bool, done: bool },
    /// TypedArray：`length` 是元素个数
    TypedArray(ElementKind, ViewData<'gc>),
    /// DataView：`length` 是字节数
    DataView(ViewData<'gc>),
}

impl ObjectKind<'_> {
    pub fn is_callable(&self) -> bool {
        matches!(self, ObjectKind::Function(_) | ObjectKind::Native(_) | ObjectKind::Bound(_) | ObjectKind::Resolver { .. } | ObjectKind::NativeClosure(_) | ObjectKind::Proxy(ProxyData { callable: true, .. }))
    }
}

/// 属性表超过这么多项才建索引
const PROP_INDEX_THRESHOLD: usize = 32;

/// 属性键 ⇒ 在属性表里的下标。
#[derive(Debug, Default, Collect)]
#[collect(require_static)]
pub struct PropIndex(Option<alloc::collections::BTreeMap<Box<str>, usize>>);

/// 对象：种类、按插入顺序的属性表、原型。
#[derive(Default, Collect)]
#[collect(no_drop)]
pub struct ObjectData<'gc> {
    pub kind: ObjectKind<'gc>,
    pub props: Vec<(JsString<'gc>, Property<'gc>)>,
    /// 属性多了以后的键 ⇒ 下标索引（少的时候直接线性找）
    pub prop_index: PropIndex,
    /// Symbol 键的属性
    pub symbols: Vec<(JsSymbol<'gc>, Property<'gc>)>,
    /// 私有成员（`#x`）：键是类求值时生成的私有名，和普通属性分开，反射看不见
    pub privates: Vec<(JsSymbol<'gc>, Property<'gc>)>,
    pub proto: Option<JsObject<'gc>>,
    /// 数组：稀疏部分撑起来的长度。稠密部分（`ObjectKind::Array` 里，全是可写可枚举可配置的数据属性、没有空洞）
    /// 之外的下标按普通属性存，实际长度取它与稠密部分长度的大者
    pub sparse_length: u32,
    /// 不可扩展（`Object.preventExtensions` / `seal` / `freeze`）
    pub non_extensible: bool,
    /// 数组的 `length` 不可写
    pub length_readonly: bool,
}

/// 只打种类与键：对象图有环（`F.prototype.constructor`），不能往下展开。
impl fmt::Debug for ObjectData<'_> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let kind = match self.kind {
            ObjectKind::Ordinary | ObjectKind::Arguments => "Object",
            ObjectKind::Error => "Error",
            ObjectKind::Array(_) => "Array",
            ObjectKind::Function(_) | ObjectKind::Native(_) | ObjectKind::Bound(_) | ObjectKind::Resolver { .. } | ObjectKind::NativeClosure(_) => "Function",
            ObjectKind::Proxy(_) => "Proxy",
            ObjectKind::ArrayBuffer(_) => "ArrayBuffer",
            ObjectKind::Date(_) => "Date",
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
    /// 数组长度；不是数组给 `None`。
    pub fn array_length(&self) -> Option<u32> {
        match &self.kind {
            ObjectKind::Array(items) => Some(u32::try_from(items.len()).unwrap_or(u32::MAX).max(self.sparse_length)),
            _ => None,
        }
    }

    /// 快路：改稠密部分里已有的第 `i` 个元素，改了给 `true`（别的情况走完整的属性写）。
    pub fn write_index(&mut self, i: u32, value: Value<'gc>) -> bool {
        match &mut self.kind {
            ObjectKind::Array(items) if (i as usize) < items.len() => {
                items[i as usize] = value;
                true
            }
            _ => false,
        }
    }

    /// 稠密部分从第 `from` 个起搬成按下标存的普通属性（要给这些元素不同的特性、或删掉其中一个时）。
    pub fn spill(&mut self, mc: &Mutation<'gc>, from: usize) {
        let ObjectKind::Array(items) = &mut self.kind else { return };
        if from >= items.len() {
            return;
        }
        let len = u32::try_from(items.len()).unwrap_or(u32::MAX);
        let moved: Vec<Value<'gc>> = items.drain(from..).collect();
        for (i, v) in (from..).zip(moved) {
            self.push_prop(mc, &alloc::format!("{i}"), Property::data(v));
        }
        self.sparse_length = self.sparse_length.max(len);
    }

    /// 数组稀疏部分的元素（稠密部分之后、按普通属性存的下标），按下标排好。
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

    /// 数组设长度：截掉 `n` 及以后的元素（含按下标存的）；变长只记长度（新的位置是空洞）。
    pub fn set_array_length(&mut self, n: u32) {
        let ObjectKind::Array(items) = &mut self.kind else { return };
        let len = n as usize;
        if len <= items.len() {
            items.truncate(len);
        }
        let dense = items.len();
        if self.sparse_length > 0 {
            self.props.retain(|(k, _)| crate::ops::index_of_str(k).is_none_or(|i| i < n));
            self.reindex();
        }
        self.sparse_length = if len > dense { n } else { 0 };
    }

    /// 自有属性（只看存着的：数组稠密部分、普通属性、Symbol 键；数组 `length` 与字符串下标由 Agent 补）。
    pub fn own_slot(&self, key: Key<'_, 'gc>) -> Option<Property<'gc>> {
        match key {
            Key::Str(k) => {
                if let (ObjectKind::Array(items), Some(i)) = (&self.kind, crate::ops::index_of_str(k))
                    && let Some(&v) = items.get(i as usize)
                {
                    return Some(Property::data(v));
                }
                self.get_own(k)
            }
            Key::Sym(s) => self.get_own_symbol(s),
        }
    }

    /// 存一个自有属性（已经验过能存）：数组的下标能进稠密部分就进，不能就按普通属性存并撑长度。
    pub fn store(&mut self, mc: &Mutation<'gc>, key: Key<'_, 'gc>, prop: Property<'gc>) {
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
                _ if at < items.len() => self.spill(mc, at),
                _ => {}
            }
        }
        self.define(mc, k, prop);
        if let (ObjectKind::Array(_), Some(i)) = (&self.kind, crate::ops::index_of_str(k)) {
            self.sparse_length = self.sparse_length.max(i.saturating_add(1));
        }
    }

    /// 删一个自有属性（不查可配置）。
    pub fn unstore(&mut self, mc: &Mutation<'gc>, key: Key<'_, 'gc>) {
        match key {
            Key::Sym(s) => self.symbols.retain(|(k, _)| !Gc::ptr_eq(*k, s)),
            Key::Str(k) => {
                if let (ObjectKind::Array(items), Some(i)) = (&self.kind, crate::ops::index_of_str(k))
                    && (i as usize) < items.len()
                {
                    self.spill(mc, i as usize);
                }
                if let Some(i) = self.position(k) {
                    self.props.remove(i);
                    self.reindex();
                }
            }
        }
    }

    /// 属性在表里的下标。
    fn position(&self, key: &str) -> Option<usize> {
        match &self.prop_index.0 {
            Some(m) => m.get(key).copied(),
            None => self.props.iter().position(|(k, _)| ***k == *key),
        }
    }

    /// 追加一项（索引跟着加，表够大了就建索引）。
    fn push_prop(&mut self, mc: &Mutation<'gc>, key: &str, prop: Property<'gc>) {
        self.props.push((Gc::new(mc, Box::from(key)), prop));
        let at = self.props.len() - 1;
        match &mut self.prop_index.0 {
            Some(m) => _ = m.insert(Box::from(key), at),
            None if self.props.len() >= PROP_INDEX_THRESHOLD => self.reindex(),
            None => {}
        }
    }

    /// 删过属性后重建索引。
    fn reindex(&mut self) {
        self.prop_index.0 = (self.props.len() >= PROP_INDEX_THRESHOLD).then(|| self.props.iter().enumerate().map(|(i, (k, _))| (Box::from(&***k), i)).collect());
    }

    /// 自有属性。
    pub fn get_own(&self, key: &str) -> Option<Property<'gc>> {
        self.position(key).map(|i| self.props[i].1)
    }

    /// 自有数据属性的值（访问器给 `None`）。
    pub fn own(&self, key: &str) -> Option<Value<'gc>> {
        self.get_own(key).and_then(|p| p.value())
    }

    /// 赋值：已有数据属性就改值（不可写就不动），没有就建一个普通属性。访问器由调用方先处理。
    pub fn set(&mut self, mc: &Mutation<'gc>, key: &str, value: Value<'gc>) {
        match self.position(key) {
            Some(i) => {
                let p = &mut self.props[i].1;
                if p.writable {
                    p.slot = Slot::Data(value);
                }
            }
            None => self.push_prop(mc, key, Property::data(value)),
        }
    }

    /// 定义：整个替换（或新建）这个属性。
    pub fn define(&mut self, mc: &Mutation<'gc>, key: &str, prop: Property<'gc>) {
        match self.position(key) {
            Some(i) => self.props[i].1 = prop,
            None => self.push_prop(mc, key, prop),
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

    /// 删自有属性：不可配置的删不掉，返回删没删成（不存在也算成）。
    pub fn remove(&mut self, key: &str) -> bool {
        match self.position(key) {
            Some(i) if self.props[i].1.configurable => {
                self.props.remove(i);
                self.reindex();
                true
            }
            Some(_) => false,
            None => true,
        }
    }

    pub fn alloc(self, mc: &Mutation<'gc>) -> JsObject<'gc> {
        Gc::new(mc, RefLock::new(self))
    }
}

impl<'gc> From<ObjectKind<'gc>> for ObjectData<'gc> {
    fn from(kind: ObjectKind<'gc>) -> Self {
        Self { kind, ..Self::default() }
    }
}
