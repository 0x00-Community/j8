use core::fmt;
use core::ops::Deref;

use alloc::collections::VecDeque;

use gc_arena::{Collect, Gc, Mutation, RefLock};

use crate::{ErrorKind, Job, JsObject, JsSymbol, Native, NativeFn, ObjectData, ObjectKind, Property, Scope, SymbolData, Value};

/// 规范里的内建 Symbol（`Symbol.iterator` 等）。
#[derive(Clone, Copy, Collect)]
#[collect(no_drop)]
pub struct WellKnown<'gc> {
    pub iterator: JsSymbol<'gc>,
    pub async_iterator: JsSymbol<'gc>,
    pub has_instance: JsSymbol<'gc>,
    pub to_primitive: JsSymbol<'gc>,
    pub to_string_tag: JsSymbol<'gc>,
}

impl<'gc> WellKnown<'gc> {
    /// 名字 ⇒ 字段，r8builtins 挂到 `Symbol` 上用。
    pub fn all(&self) -> [(&'static str, JsSymbol<'gc>); 5] {
        [("iterator", self.iterator), ("asyncIterator", self.async_iterator), ("hasInstance", self.has_instance), ("toPrimitive", self.to_primitive), ("toStringTag", self.to_string_tag)]
    }
}

/// 一个执行环境的全部内建对象：全局对象与各原型。r8frame 只建空壳并连好原型链，
/// 方法由 [`Intrinsics`] 的实现（r8builtins）装进去。
#[derive(Collect)]
#[collect(no_drop)]
pub struct Realm<'gc> {
    pub global: JsObject<'gc>,
    pub object_proto: JsObject<'gc>,
    pub function_proto: JsObject<'gc>,
    pub array_proto: JsObject<'gc>,
    pub string_proto: JsObject<'gc>,
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
    pub promise_proto: JsObject<'gc>,
    pub symbols: WellKnown<'gc>,
    /// 微任务队列
    pub jobs: Gc<'gc, RefLock<VecDeque<Job<'gc>>>>,
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
        Self {
            symbol_proto: derived(object_proto),
            iterator_proto,
            array_iterator_proto: derived(iterator_proto),
            string_iterator_proto: derived(iterator_proto),
            generator_proto: derived(iterator_proto),
            promise_proto: derived(object_proto),
            symbols: WellKnown { iterator: symbol("iterator"), async_iterator: symbol("asyncIterator"), has_instance: symbol("hasInstance"), to_primitive: symbol("toPrimitive"), to_string_tag: symbol("toStringTag") },
            jobs: Gc::new(mc, RefLock::new(VecDeque::new())),
            global: derived(object_proto),
            object_proto,
            function_proto: derived(object_proto),
            array_proto: derived(object_proto),
            string_proto: derived(object_proto),
            number_proto: derived(object_proto),
            boolean_proto: derived(object_proto),
            error_protos,
        }
    }

    pub fn error_proto(&self, kind: ErrorKind) -> JsObject<'gc> {
        self.error_protos[kind as usize]
    }

    /// 一个宿主函数对象（原型是 `Function.prototype`）。
    pub fn native(&self, mc: &Mutation<'gc>, f: NativeFn) -> JsObject<'gc> {
        ObjectData { kind: ObjectKind::Native(Native(f)), proto: Some(self.function_proto), ..ObjectData::default() }.alloc(mc)
    }

    /// 在 `target` 上挂一个以 Symbol 为键的宿主方法。
    pub fn symbol_method(&self, mc: &Mutation<'gc>, target: JsObject<'gc>, key: JsSymbol<'gc>, f: NativeFn) {
        let func = self.native(mc, f);
        target.borrow_mut(mc).define_symbol(key, Property::hidden(Value::Object(func)));
    }

    /// 在 `target` 上挂一个宿主方法。
    pub fn method(&self, mc: &Mutation<'gc>, target: JsObject<'gc>, name: &str, f: NativeFn) {
        let func = self.native(mc, f);
        target.borrow_mut(mc).define(mc, name, Property::hidden(Value::Object(func)));
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

    /// 在 `cx` 里用 `engine` 跑一段脚本。
    pub fn run(&mut self, cx: Context<'gc>, engine: &mut impl crate::Engine, src: &crate::Source<'_>) -> Result<Value<'gc>, crate::Error> {
        engine.eval(self, cx, src)
    }
}
