use core::fmt;
use core::ops::Deref;

use gc_arena::{Collect, Gc, Mutation};

use crate::{ErrorKind, JsObject, Native, NativeFn, ObjectData, ObjectKind, Property, Scope, Value};

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
        Self {
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
