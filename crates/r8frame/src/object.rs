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
    /// `then(onFulfilled, onRejected)`，结果交给 `derived`
    Then { on_fulfilled: Value<'gc>, on_rejected: Value<'gc>, derived: Option<JsObject<'gc>> },
    /// `await`：恢复这个协程
    Await { coroutine: JsObject<'gc> },
    /// `Promise.all` 的第 `index` 个：兑现就填进 `values`，`remaining` 减到 0 兑现 `result`；拒绝就拒绝 `result`
    All { values: JsObject<'gc>, index: u32, remaining: Gc<'gc, Lock<u32>>, result: JsObject<'gc> },
    /// `finally(f)`：调 `f()`，`derived` 照原样落定（`f` 抛错就拒绝）
    Finally { on_finally: Value<'gc>, derived: JsObject<'gc> },
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
    CollectionIterator { target: Option<JsObject<'gc>>, index: usize, kind: IterKind },
}

impl ObjectKind<'_> {
    pub fn is_callable(&self) -> bool {
        matches!(self, ObjectKind::Function(_) | ObjectKind::Native(_) | ObjectKind::Bound(_) | ObjectKind::Resolver { .. })
    }
}

/// 对象：种类、按插入顺序的属性表、原型。
#[derive(Default, Collect)]
#[collect(no_drop)]
pub struct ObjectData<'gc> {
    pub kind: ObjectKind<'gc>,
    pub props: Vec<(JsString<'gc>, Property<'gc>)>,
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
            ObjectKind::Function(_) | ObjectKind::Native(_) | ObjectKind::Bound(_) | ObjectKind::Resolver { .. } => "Function",
            ObjectKind::Iterator(_) | ObjectKind::CollectionIterator { .. } => "Iterator",
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
            self.props.push((Gc::new(mc, Box::from(alloc::format!("{i}").as_str())), Property::data(v)));
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
        if self.sparse_length > 0 {
            self.props.retain(|(k, _)| crate::ops::index_of_str(k).is_none_or(|i| i < n));
        }
        self.sparse_length = if len > items.len() { n } else { 0 };
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
                self.props.retain(|(n, _)| ***n != *k);
            }
        }
    }

    /// 自有属性。
    pub fn get_own(&self, key: &str) -> Option<Property<'gc>> {
        self.props.iter().find_map(|&(k, p)| (**k == *key).then_some(p))
    }

    /// 自有数据属性的值（访问器给 `None`）。
    pub fn own(&self, key: &str) -> Option<Value<'gc>> {
        self.get_own(key).and_then(|p| p.value())
    }

    /// 赋值：已有数据属性就改值（不可写就不动），没有就建一个普通属性。访问器由调用方先处理。
    pub fn set(&mut self, mc: &Mutation<'gc>, key: &str, value: Value<'gc>) {
        match self.props.iter_mut().find(|(k, _)| ***k == *key) {
            Some((_, p)) => {
                if p.writable {
                    p.slot = Slot::Data(value);
                }
            }
            None => self.props.push((Gc::new(mc, Box::from(key)), Property::data(value))),
        }
    }

    /// 定义：整个替换（或新建）这个属性。
    pub fn define(&mut self, mc: &Mutation<'gc>, key: &str, prop: Property<'gc>) {
        match self.props.iter_mut().find(|(k, _)| ***k == *key) {
            Some((_, p)) => *p = prop,
            None => self.props.push((Gc::new(mc, Box::from(key)), prop)),
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
        match self.props.iter().position(|(k, _)| ***k == *key) {
            Some(i) if self.props[i].1.configurable => {
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
