use alloc::boxed::Box;
use alloc::vec::Vec;
use core::fmt;

use r8gc::{Collect, Gc, Lock, Mutation, RefLock};

use crate::{Agent, Error, FuncId, Key, Value};

/// 堆上的字符串。
pub type JsString<'gc> = Gc<'gc, SmallStr>;

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
    pub compiled: Gc<'gc, r8gc::Static<Box<dyn core::any::Any>>>,
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
    /// 执行层可以不进宿主、直接展开成对目标的调用（`call` / `apply`）
    pub forward: Forward,
}

/// 只是换个方式调别的函数的内建函数。执行层把它展开成普通调用，被调的 JS 留在执行层自己的帧栈上
/// （不在宿主函数里重入，照样分片、回收）。
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Forward {
    #[default]
    No,
    /// `Function.prototype.call`：`this` 是目标，第一个实参是新的 `this`，其余照传
    Call,
    /// `Function.prototype.apply`：`this` 是目标，第一个实参是新的 `this`，第二个是类数组实参表
    Apply,
}

impl Native {
    /// 普通宿主函数（不能 `new`）。
    pub fn function(f: NativeFn) -> Self {
        Self { f, constructor: false, forward: Forward::No }
    }

    /// 内建构造器（能 `new`）。
    pub fn constructor(f: NativeFn) -> Self {
        Self { f, constructor: true, forward: Forward::No }
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

/// Map / Set 按 SameValueZero 找键用的键：`NaN` 只有一个、`-0` 当 `+0`，字符串与 BigInt 比内容，对象与 Symbol 比身份。
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum MapKey {
    Undefined,
    Null,
    Bool(bool),
    Number(u64),
    /// 字符串值本身（短串内联、长串共享，查找时不分配）
    String(SmallStr),
    BigInt(Box<str>),
    /// 对象 / Symbol 的地址（回收会搬对象：搬过之后 `Collection` 重建索引）
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
            Value::String(s) => MapKey::String((*s).clone()),
            Value::BigInt(b) => MapKey::BigInt(alloc::format!("{}", b.0).into()),
            Value::Symbol(s) => MapKey::Ref(Gc::as_ptr(s) as usize),
            Value::Object(o) => MapKey::Ref(Gc::as_ptr(o).cast::<u8>() as usize),
        }
    }
}

/// Map / Set 的存储：按插入顺序的条目（删掉的留空位，迭代中删改也能接着走）加按键找下标的索引。
/// 索引按对象地址找键，回收搬了键之后重建（手写 `Collect`）。
#[derive(Debug)]
pub struct Collection<'gc> {
    entries: Vec<Option<(Value<'gc>, Value<'gc>)>>,
    /// 自上次次回收以来，可能写进了新生代指针的条目从这里开始（追加的天然在它后面）
    young_from: usize,
    index: r8gc::Static<hashbrown::HashMap<MapKey, usize, core::hash::BuildHasherDefault<rustc_hash::FxHasher>>>,
    pub size: usize,
    pub weak: bool,
}

unsafe impl<'gc> Collect<'gc> for Collection<'gc> {
    fn trace(&self, t: &mut r8gc::Trace<'_>) {
        t.trace(&self.entries);
    }

    /// 搬了的键（对象 / Symbol，按地址进索引）才在索引里换成新地址。
    /// 搬前的地址（新生代 / 要搬空的页）与搬后的不重叠，先删后插不会碰到别的键。
    /// 次回收只看上次以来写过的条目（更早的只指向老年代，不会变）。
    fn relocate(&mut self, r: &mut r8gc::Relocate<'_>) {
        let from = if r.young_only() { self.young_from } else { 0 };
        self.young_from = self.entries.len();
        for (i, entry) in self.entries.iter_mut().enumerate().skip(from) {
            let Some((k, v)) = entry else { continue };
            let old = matches!(k, Value::Object(_) | Value::Symbol(_)).then(|| MapKey::from(*k));
            r.relocate(k);
            r.relocate(v);
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
        Self { entries: Vec::new(), young_from: 0, index: r8gc::Static(hashbrown::HashMap::default()), size: 0, weak }
    }

    /// 按插入顺序的条目（删掉的是空位）
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

    /// 设值：已有就改，否则追加到末尾。`-0` 键存成 `+0`。
    pub fn set(&mut self, key: Value<'gc>, value: Value<'gc>) {
        // `-0 + 0` 是 `+0`
        let key = match key {
            Value::Number(x) => Value::Number(x + 0.0),
            k => k,
        };
        match self.index.0.get(&MapKey::from(key)) {
            Some(&i) => {
                self.entries[i] = Some((key, value));
                self.young_from = self.young_from.min(i);
            }
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

/// 映射的 arguments：形参所在的环境与每个下标对应的槽位（删掉 / 改成访问器后就不再映射）。
#[derive(Clone, Debug, Collect)]
#[collect(no_drop)]
pub struct ArgumentsMap<'gc> {
    pub env: JsEnv<'gc>,
    pub slots: Vec<Option<u16>>,
}

/// 宿主对象里的 Rust 数据（按类型取出，可变借用经 `RefCell`）。
pub type HostData<'gc> = Gc<'gc, r8gc::Static<core::cell::RefCell<Box<dyn core::any::Any>>>>;

/// 对象的种类：普通对象之外带的内部槽。
#[derive(Debug, Default, Collect)]
#[collect(no_drop)]
pub enum ObjectKind<'gc> {
    #[default]
    Ordinary,
    /// arguments 对象（`Object.prototype.toString` 认它）；非严格、简单形参时第 i 个下标映射到形参的环境槽位
    Arguments(Option<ArgumentsMap<'gc>>),
    /// Error 实例（有 [[ErrorData]]）
    Error,
    /// 数组元素（下标属性存在这里，`length` 就是它的长度）
    Array(Vec<Value<'gc>>),
    Function(Closure<'gc>),
    Native(Native),
    Bound(Box<Bound<'gc>>),
    Iterator(IteratorState<'gc>),
    Coroutine(Box<Coroutine<'gc>>),
    Promise { state: Settled<'gc>, reactions: Vec<Reaction<'gc>> },
    /// Promise 的 resolve / reject 函数：共享一个「已经解决过」的标记
    Resolver { promise: JsObject<'gc>, reject: bool, done: Gc<'gc, Lock<bool>> },
    RegExp(RegExpData<'gc>),
    /// 原始值包装对象（`new Boolean(…)` / `new Number(…)` / `new String(…)` / `Object(sym)` …）：存着被包的原始值
    Primitive(Value<'gc>),
    /// Map（`weak` 为真是 WeakMap）
    Map(Box<Collection<'gc>>),
    /// Set（值放在键与值两处；`weak` 为真是 WeakSet）
    Set(Box<Collection<'gc>>),
    /// Map / Set 的迭代器：活视图，按条目下标走（`target` 为 `None` 就是走完了）
    CollectionIterator { target: Option<JsObject<'gc>>, index: usize, kind: IterKind, set: bool },
    NativeClosure(NativeClosure<'gc>),
    Proxy(ProxyData<'gc>),
    IteratorHelper(Box<IteratorHelper<'gc>>),
    /// `Iterator.from` 包出来的迭代器
    WrappedIterator(crate::IterRecord<'gc>),
    ArrayBuffer(Box<BufferData>),
    /// Date：时间值（毫秒，无效是 NaN）
    Date(f64),
    /// 宿主对象：绑定层（r8binding）放进来的 Rust 数据（不含 GC 指针，按类型取出）
    Host(HostData<'gc>),
    /// 模块命名空间对象：导出名（排好序）与解析到的绑定
    ModuleNamespace(Vec<(JsString<'gc>, crate::ResolvedBinding<'gc>)>),
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
    /// 自有属性就是属性表里的那些：[[GetOwnProperty]] / [[GetPrototypeOf]] 都是普通的
    /// （不是 Proxy / 数组 / 类型化数组 / 模块命名空间 / 字符串包装 / arguments），查属性可以走快路。
    pub fn has_ordinary_properties(&self) -> bool {
        !matches!(
            self,
            ObjectKind::Proxy(_) | ObjectKind::Array(_) | ObjectKind::TypedArray(..) | ObjectKind::ModuleNamespace(_) | ObjectKind::Primitive(Value::String(_)) | ObjectKind::Arguments(_)
        )
    }

    pub fn is_callable(&self) -> bool {
        matches!(self, ObjectKind::Function(_) | ObjectKind::Native(_) | ObjectKind::Bound(_) | ObjectKind::Resolver { .. } | ObjectKind::NativeClosure(_) | ObjectKind::Proxy(ProxyData { callable: true, .. }))
    }
}


pub use crate::text::SmallStr;

/// 多数对象没有的一串东西（Symbol 键属性、私有成员）：空着只占一个指针。
#[derive(Collect)]
#[collect(no_drop)]
#[expect(clippy::box_collection, reason = "装箱就是为了空着时只占一个指针（Vec 本身要三个字）")]
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
}

/// 对象：种类、按插入顺序的属性表、原型。
#[derive(Default, Collect)]
#[collect(no_drop)]
pub struct ObjectData<'gc> {
    pub kind: ObjectKind<'gc>,
    /// 字符串键的属性：形状（隐藏类）加槽，按插入顺序
    pub props: crate::Props<'gc>,
    /// 以这个对象为原型的对象从哪个空形状起步（同原型、同样加属性的对象因此共享形状）。没有 `Gc`，改它不用写屏障
    #[collect(require_static)]
    pub shape_root: core::cell::RefCell<Option<alloc::rc::Rc<crate::Shape>>>,
    /// Symbol 键的属性
    pub symbols: Few<(JsSymbol<'gc>, Property<'gc>)>,
    /// 私有成员（`#x`）：键是类求值时生成的私有名，和普通属性分开，反射看不见
    pub privates: Few<(JsSymbol<'gc>, Property<'gc>)>,
    pub proto: Option<JsObject<'gc>>,
    /// 数组：稀疏部分撑起来的长度。稠密部分（`ObjectKind::Array` 里，全是可写可枚举可配置的数据属性、没有空洞）
    /// 之外的下标按普通属性存，实际长度取它与稠密部分长度的大者
    pub sparse_length: u32,
    /// 不可扩展（`Object.preventExtensions` / `seal` / `freeze`）
    pub non_extensible: bool,
    /// 数组的 `length` 不可写
    pub length_readonly: bool,
    /// 属性表里有过数字开头的键（只置不清；数组方法的快路靠它一眼确认原型上没有下标属性）
    pub digit_keys: bool,
}

/// 只打种类与键：对象图有环（`F.prototype.constructor`），不能往下展开。
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
            self.props.retain(|k| crate::ops::index_of_str(k).is_none_or(|i| i < n));
        }
        self.sparse_length = if len > dense { n } else { 0 };
    }

    /// 自有属性（只看存着的：数组稠密部分、普通属性、Symbol 键；数组 `length` 与字符串下标由 Agent 补）。
    pub fn own_slot(&self, key: Key<'_, 'gc>) -> Option<Property<'gc>> {
        match key {
            Key::Str(k) => {
                // 先看是不是数组再解析下标：普通对象查属性不用解析
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

    /// 存一个自有属性（已经验过能存）：数组的下标能进稠密部分就进，不能就按普通属性存并撑长度。
    /// 普通对象写属性的快路（OrdinarySet 在接收者自己身上的那一步）：自有可写数据属性就改值，
    /// 没有且可扩展就新建普通数据属性。给出 `Some(成没成)`；要走慢路（有访问器 / 不是普通对象）给 `None`。
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

    /// 删一个自有属性（不查可配置）。
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

    /// 属性在表里的下标。
    /// 属性表第 `at` 项正好是 `key` 的话给出它（执行层缓存了下标，用之前核对）。
    pub fn prop_at(&self, at: usize, key: &str) -> Option<Property<'gc>> {
        (at < self.props.len() && **self.props.key(at) == *key).then(|| self.props.get(at))
    }

    /// 改属性表第 `at` 项的值（调用方已用 [`prop_at`](Self::prop_at) 核对过是可写数据属性）。
    pub fn set_at(&mut self, at: usize, value: Value<'gc>) {
        if at < self.props.len() {
            *self.props.slot_mut(at) = Slot::Data(value);
        }
    }

    pub fn position(&self, key: &str) -> Option<usize> {
        self.props.position(key)
    }

    /// 追加一项（形状沿转移表走）。
    fn push_prop(&mut self, key: &str, prop: Property<'gc>) {
        self.digit_keys |= key.as_bytes().first().is_some_and(u8::is_ascii_digit);
        let proto = self.proto;
        self.props.push(key, prop, || proto.and_then(|p| p.try_borrow().ok().map(|d| d.root_shape())));
    }

    /// 以这个对象为原型的对象起步的空形状（没有就建）
    pub fn root_shape(&self) -> alloc::rc::Rc<crate::Shape> {
        self.shape_root.borrow_mut().get_or_insert_with(crate::Props::empty).clone()
    }

    /// 追加一项，形状直接换成 `to`（执行层的内联缓存给的：从当前形状加 `key` 就是它）。
    pub fn push_to(&mut self, key: &str, to: alloc::rc::Rc<crate::Shape>, slot: Slot<'gc>) {
        self.digit_keys |= key.as_bytes().first().is_some_and(u8::is_ascii_digit);
        self.props.push_to(to, slot);
    }

    /// 自有属性。
    pub fn get_own(&self, key: &str) -> Option<Property<'gc>> {
        self.position(key).map(|i| self.props.get(i))
    }

    /// 自有数据属性的值（访问器给 `None`）。
    pub fn own(&self, key: &str) -> Option<Value<'gc>> {
        self.get_own(key).and_then(|p| p.value())
    }

    /// 赋值：已有数据属性就改值（不可写就不动），没有就建一个普通属性。访问器由调用方先处理。
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

    /// 定义：整个替换（或新建）这个属性。
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

    /// 删自有属性：不可配置的删不掉，返回删没删成（不存在也算成）。
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
        Gc::new(mc, RefLock::new(self))
    }
}

impl<'gc> From<ObjectKind<'gc>> for ObjectData<'gc> {
    fn from(kind: ObjectKind<'gc>) -> Self {
        Self { kind, ..Self::default() }
    }
}
