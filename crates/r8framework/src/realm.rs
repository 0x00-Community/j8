use core::fmt;
use core::ops::Deref;

use alloc::collections::VecDeque;

use r8gc::{Collect, Gc, Mutation, RefLock};

use crate::{ErrorKind, Job, JsObject, JsString, JsSymbol, Native, NativeFn, ObjectData, ObjectKind, Property, Scope, SymbolData, Value};

/// 规范里的内建 Symbol（`Symbol.iterator` 等）。
#[derive(Clone, Copy, Collect)]
#[collect(no_drop)]
pub struct WellKnown<'gc> {
    pub iterator: JsSymbol<'gc>,
    pub async_iterator: JsSymbol<'gc>,
    pub has_instance: JsSymbol<'gc>,
    pub to_primitive: JsSymbol<'gc>,
    pub to_string_tag: JsSymbol<'gc>,
    pub species: JsSymbol<'gc>,
    pub unscopables: JsSymbol<'gc>,
    pub is_concat_spreadable: JsSymbol<'gc>,
    pub match_: JsSymbol<'gc>,
    pub match_all: JsSymbol<'gc>,
    pub replace: JsSymbol<'gc>,
    pub search: JsSymbol<'gc>,
    pub split: JsSymbol<'gc>,
}

impl<'gc> WellKnown<'gc> {
    /// 名字 ⇒ 字段，r8builtins 挂到 `Symbol` 上用。
    pub fn all(&self) -> [(&'static str, JsSymbol<'gc>); 13] {
        [
            ("iterator", self.iterator),
            ("asyncIterator", self.async_iterator),
            ("hasInstance", self.has_instance),
            ("toPrimitive", self.to_primitive),
            ("toStringTag", self.to_string_tag),
            ("species", self.species),
            ("unscopables", self.unscopables),
            ("isConcatSpreadable", self.is_concat_spreadable),
            ("match", self.match_),
            ("matchAll", self.match_all),
            ("replace", self.replace),
            ("search", self.search),
            ("split", self.split),
        ]
    }
}

/// 一个执行环境的全部内建对象：全局对象与各原型。r8framework 只建空壳并连好原型链，
/// 方法由 [`Intrinsics`] 的实现（r8builtins）装进去。
#[derive(Collect)]
#[collect(no_drop)]
pub struct Realm<'gc> {
    pub global: JsObject<'gc>,
    pub object_proto: JsObject<'gc>,
    pub function_proto: JsObject<'gc>,
    pub array_proto: JsObject<'gc>,
    pub string_proto: JsObject<'gc>,
    /// ASCII 单字符的字符串（`s[i]` / `charAt` 不用每次分配）
    pub chars: alloc::vec::Vec<crate::JsString<'gc>>,
    pub number_proto: JsObject<'gc>,
    pub boolean_proto: JsObject<'gc>,
    /// 按 [`ErrorKind`] 排的错误原型（`Error.prototype` 在第一个）
    pub error_protos: [JsObject<'gc>; ErrorKind::ALL.len()],
    pub symbol_proto: JsObject<'gc>,
    /// %IteratorPrototype%：`[Symbol.iterator]() { return this }`
    pub iterator_proto: JsObject<'gc>,
    pub array_iterator_proto: JsObject<'gc>,
    pub string_iterator_proto: JsObject<'gc>,
    /// %GeneratorPrototype%：`next` / `return` / `throw`
    pub generator_proto: JsObject<'gc>,
    /// %GeneratorFunction.prototype%：生成器函数对象的原型
    pub generator_function_proto: JsObject<'gc>,
    /// %AsyncFunction.prototype%
    pub async_function_proto: JsObject<'gc>,
    /// %AsyncGeneratorFunction.prototype%
    pub async_generator_function_proto: JsObject<'gc>,
    /// %AsyncIteratorPrototype%：`[Symbol.asyncIterator]() { return this }`
    pub async_iterator_proto: JsObject<'gc>,
    /// %AsyncGeneratorPrototype%：`next` / `return` / `throw` 都返回 Promise
    pub async_generator_proto: JsObject<'gc>,
    pub promise_proto: JsObject<'gc>,
    /// %Promise%（r8builtins 把它装成构造器）
    pub promise_ctor: JsObject<'gc>,
    pub regexp_proto: JsObject<'gc>,
    /// 正则字面量用的构造器：r8builtins 换上真的实现（用户改写全局 `RegExp` 不影响字面量）
    pub regexp_ctor: JsObject<'gc>,
    /// %eval%：严格代码里直接调用它时按直接 eval 处理
    pub eval: JsObject<'gc>,
    pub bigint_proto: JsObject<'gc>,
    pub map_proto: JsObject<'gc>,
    pub set_proto: JsObject<'gc>,
    pub weakmap_proto: JsObject<'gc>,
    pub weakset_proto: JsObject<'gc>,
    /// Map / Set 迭代器的原型（`next`）
    pub collection_iterator_proto: JsObject<'gc>,
    /// %SetIteratorPrototype%（`collection_iterator_proto` 是 Map 的）
    pub set_iterator_proto: JsObject<'gc>,
    /// %IteratorHelperPrototype%
    pub iterator_helper_proto: JsObject<'gc>,
    /// %WrapForValidIteratorPrototype%
    pub wrap_iterator_proto: JsObject<'gc>,
    pub date_proto: JsObject<'gc>,
    pub weak_ref_proto: JsObject<'gc>,
    pub finalization_registry_proto: JsObject<'gc>,
    /// %RegExpStringIteratorPrototype%
    pub regexp_string_iterator_proto: JsObject<'gc>,
    pub array_buffer_proto: JsObject<'gc>,
    pub shared_array_buffer_proto: JsObject<'gc>,
    pub data_view_proto: JsObject<'gc>,
    /// %TypedArray% 与 %TypedArray.prototype%
    pub typed_array_ctor: JsObject<'gc>,
    pub typed_array_proto: JsObject<'gc>,
    /// 各具体 TypedArray 的构造器与原型（按 [`ElementKind::ALL`](crate::ElementKind::ALL) 排）
    pub typed_ctors: [JsObject<'gc>; 12],
    pub typed_protos: [JsObject<'gc>; 12],
    /// 脚本作用域：各脚本顶层的 `let` / `const` / `class`（跨脚本共享，不是全局对象的属性）
    pub script_scope: JsObject<'gc>,
    pub symbols: WellKnown<'gc>,
    /// 加载过的模块（编号就是下标）
    pub modules: Gc<'gc, RefLock<alloc::vec::Vec<crate::ModuleRecord<'gc>>>>,
    /// `Symbol.for` 的全局登记表
    pub symbol_registry: Gc<'gc, RefLock<alloc::vec::Vec<(JsString<'gc>, JsSymbol<'gc>)>>>,
    /// `Math.random` 的状态（xorshift，0 表示还没播种）
    pub random_state: Gc<'gc, r8gc::Lock<u64>>,
    /// 微任务队列
    pub jobs: Gc<'gc, RefLock<VecDeque<Job<'gc>>>>,
    /// 标签模板的模板对象：按（函数编号, 调用点）缓存，同一处每次同一个
    pub templates: Gc<'gc, RefLock<alloc::collections::BTreeMap<(u32, u32), JsObject<'gc>>>>,
    /// 编好的正则按（源码, 标志）共享（编好的不可变）；最近用的放后面，满了丢最前面的
    pub regexp_cache: Gc<'gc, RefLock<VecDeque<RegExpCompiled<'gc>>>>,
    /// `RegExp.prototype` 装好时的样子（内建正则方法据此判断能不能跳过协议直接匹配）
    pub regexp_pristine: Gc<'gc, RefLock<Option<RegExpPristine<'gc>>>>,
}

/// `RegExp.prototype` 装好时的形状，及 `exec` / `flags` / 各标志 getter 所在的槽与原来的内容。
#[derive(Collect)]
#[collect(no_drop)]
pub struct RegExpPristine<'gc> {
    #[collect(require_static)]
    pub shape: alloc::rc::Rc<crate::Shape>,
    pub slots: alloc::vec::Vec<(usize, crate::Slot<'gc>)>,
}

/// 一份编好的正则：源码、标志、执行层的编译结果。
#[derive(Clone, Collect)]
#[collect(no_drop)]
pub struct RegExpCompiled<'gc> {
    pub source: crate::SmallStr,
    pub flags: crate::SmallStr,
    pub compiled: Gc<'gc, r8gc::Static<alloc::boxed::Box<dyn core::any::Any>>>,
}

impl fmt::Debug for Realm<'_> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("Realm")
    }
}

impl<'gc> Realm<'gc> {
    fn new(mc: &Mutation<'gc>) -> Self {
        let object_proto = ObjectData::default().alloc(mc);
        let derived = |proto: JsObject<'gc>| ObjectData { proto: Some(proto), ..ObjectData::default() }.alloc(mc);
        let error = derived(object_proto);
        let error_protos = ErrorKind::ALL.map(|k| if k == ErrorKind::Error { error } else { derived(error) });
        let symbol = |d: &str| Gc::new(mc, SymbolData { description: Some(alloc::format!("Symbol.{d}").into()) });
        let iterator_proto = derived(object_proto);
        let async_iterator_proto = derived(object_proto);
        let function_proto = derived(object_proto);
        let placeholder = |proto: JsObject<'gc>| ObjectData { kind: ObjectKind::Native(Native::function(Self::no_regexp)), proto: Some(proto), ..ObjectData::default() }.alloc(mc);
        let typed_array_ctor = placeholder(function_proto);
        let typed_array_proto = derived(object_proto);
        Self {
            date_proto: derived(object_proto),
            weak_ref_proto: derived(object_proto),
            finalization_registry_proto: derived(object_proto),
            regexp_string_iterator_proto: derived(iterator_proto),
            array_buffer_proto: derived(object_proto),
            shared_array_buffer_proto: derived(object_proto),
            data_view_proto: derived(object_proto),
            typed_array_ctor,
            typed_array_proto,
            typed_ctors: [(); 12].map(|()| placeholder(typed_array_ctor)),
            typed_protos: [(); 12].map(|()| derived(typed_array_proto)),
            symbol_proto: derived(object_proto),
            iterator_proto,
            array_iterator_proto: derived(iterator_proto),
            string_iterator_proto: derived(iterator_proto),
            generator_proto: derived(iterator_proto),
            async_iterator_proto,
            generator_function_proto: derived(function_proto),
            async_function_proto: derived(function_proto),
            async_generator_function_proto: derived(function_proto),
            async_generator_proto: derived(async_iterator_proto),
            promise_proto: derived(object_proto),
            promise_ctor: placeholder(function_proto),
            regexp_proto: derived(object_proto),
            regexp_ctor: ObjectData { kind: ObjectKind::Native(Native::function(Self::no_regexp)), ..ObjectData::default() }.alloc(mc),
            eval: ObjectData { kind: ObjectKind::Native(Native::function(Self::no_eval)), proto: Some(function_proto), ..ObjectData::default() }.alloc(mc),
            bigint_proto: derived(object_proto),
            map_proto: derived(object_proto),
            set_proto: derived(object_proto),
            weakmap_proto: derived(object_proto),
            weakset_proto: derived(object_proto),
            collection_iterator_proto: derived(iterator_proto),
            set_iterator_proto: derived(iterator_proto),
            iterator_helper_proto: derived(iterator_proto),
            wrap_iterator_proto: derived(iterator_proto),
            script_scope: ObjectData::default().alloc(mc),
            symbols: WellKnown {
                iterator: symbol("iterator"),
                async_iterator: symbol("asyncIterator"),
                has_instance: symbol("hasInstance"),
                to_primitive: symbol("toPrimitive"),
                to_string_tag: symbol("toStringTag"),
                species: symbol("species"),
                unscopables: symbol("unscopables"),
                is_concat_spreadable: symbol("isConcatSpreadable"),
                match_: symbol("match"),
                match_all: symbol("matchAll"),
                replace: symbol("replace"),
                search: symbol("search"),
                split: symbol("split"),
            },
            jobs: Gc::new(mc, RefLock::new(VecDeque::new())),
            regexp_cache: Gc::new(mc, RefLock::new(VecDeque::new())),
            regexp_pristine: Gc::new(mc, RefLock::new(None)),
            symbol_registry: Gc::new(mc, RefLock::new(alloc::vec::Vec::new())),
            modules: Gc::new(mc, RefLock::new(alloc::vec::Vec::new())),
            random_state: Gc::new(mc, r8gc::Lock::new(0)),
            templates: Gc::new(mc, RefLock::new(alloc::collections::BTreeMap::new())),
            global: derived(object_proto),
            object_proto,
            function_proto,
            array_proto: derived(object_proto),
            string_proto: derived(object_proto),
            chars: (0u8..128).map(|c| Gc::new(mc, crate::JsStr::from(alloc::string::String::from(char::from(c)).as_str()))).collect(),
            number_proto: derived(object_proto),
            boolean_proto: derived(object_proto),
            error_protos,
        }
    }

    /// 没装正则引擎时正则字面量抛 SyntaxError。
    fn no_regexp<'g>(a: &mut crate::Agent<'g, '_>, _: Value<'g>, _: &[Value<'g>]) -> Result<Value<'g>, crate::Error> {
        Err(a.throw(ErrorKind::Syntax, "regular expressions are not available in this realm"))
    }

    fn no_eval<'g>(a: &mut crate::Agent<'g, '_>, _: Value<'g>, _: &[Value<'g>]) -> Result<Value<'g>, crate::Error> {
        Err(a.throw(ErrorKind::Type, "eval is not available in this realm"))
    }

    pub fn error_proto(&self, kind: ErrorKind) -> JsObject<'gc> {
        self.error_protos[kind as usize]
    }

    /// 一个宿主函数对象（原型是 `Function.prototype`）。
    pub fn native(&self, mc: &Mutation<'gc>, f: NativeFn) -> JsObject<'gc> {
        ObjectData { kind: ObjectKind::Native(Native::function(f)), proto: Some(self.function_proto), ..ObjectData::default() }.alloc(mc)
    }

    /// 给函数对象定义 `name`（与可选的 `length`）：不可枚举、不可写、可配置，`length` 在前。
    pub fn name_function(&self, mc: &Mutation<'gc>, f: JsObject<'gc>, name: &str, length: Option<u32>) {
        let prop = |v: Value<'gc>| Property { slot: crate::Slot::Data(v), enumerable: false, writable: false, configurable: true };
        let mut d = f.borrow_mut(mc);
        if let Some(n) = length {
            d.define("length", prop(f64::from(n).into()));
        }
        d.define("name", prop(Value::String(Gc::new(mc, crate::JsStr::from(name)))));
    }

    /// 带 `name` 的宿主函数对象。
    pub fn named_native(&self, mc: &Mutation<'gc>, f: NativeFn, name: &str, length: u32) -> JsObject<'gc> {
        let func = self.native(mc, f);
        self.name_function(mc, func, name, Some(length));
        func
    }

    /// 在 `target` 上挂一个以 Symbol 为键的宿主方法（名字是 `[描述]`）。
    pub fn symbol_method(&self, mc: &Mutation<'gc>, target: JsObject<'gc>, key: JsSymbol<'gc>, f: NativeFn, length: u32) {
        let func = self.named_native(mc, f, &alloc::format!("[{}]", key.description.as_deref().unwrap_or("")), length);
        target.borrow_mut(mc).define_symbol(key, Property::hidden(Value::Object(func)));
    }

    /// 在 `target` 上挂一个宿主方法。
    pub fn method(&self, mc: &Mutation<'gc>, target: JsObject<'gc>, name: &str, f: NativeFn, length: u32) {
        let func = self.named_native(mc, f, name, length);
        target.borrow_mut(mc).define(name, Property::hidden(Value::Object(func)));
    }
}

/// 执行环境：指向一个 [`Realm`]。要跨 `enter` 保留，把 `global` 存成 [`Global`](crate::Global)。
#[derive(Clone, Copy, Debug, Collect)]
#[collect(no_drop)]
pub struct Context<'gc>(Gc<'gc, Realm<'gc>>);

impl<'gc> Deref for Context<'gc> {
    type Target = Realm<'gc>;

    fn deref(&self) -> &Realm<'gc> {
        &self.0
    }
}

/// 往新建的 [`Realm`] 里装内建对象（r8builtins 实现）。
pub trait Intrinsics {
    fn install<'gc>(&self, mc: &Mutation<'gc>, realm: &Realm<'gc>);
}

impl<'gc> Scope<'gc, '_> {
    /// 新建一个执行环境并装好内建对象。
    pub fn context(&self, intrinsics: &dyn Intrinsics) -> Context<'gc> {
        let realm = Realm::new(self.mc);
        intrinsics.install(self.mc, &realm);
        Context(Gc::new(self.mc, realm))
    }

}
