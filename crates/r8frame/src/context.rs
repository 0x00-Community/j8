use gc_arena::Collect;

use crate::{Engine, Error, JsObject, ObjectData, Scope, Source, Value};

/// 执行环境：一个全局对象。要跨 `enter` 保留，把 `global` 存成 [`Global`](crate::Global)。
#[derive(Clone, Copy, Debug, Collect)]
#[collect(no_drop)]
pub struct Context<'gc> {
    pub global: JsObject<'gc>,
}

impl<'gc> From<JsObject<'gc>> for Context<'gc> {
    fn from(global: JsObject<'gc>) -> Self {
        Self { global }
    }
}

impl<'gc> Scope<'gc, '_> {
    /// 新建一个带全新全局对象的执行环境。
    pub fn context(&self) -> Context<'gc> {
        ObjectData::default().alloc(self.mc).into()
    }

    /// 在 `cx` 里用 `engine` 跑一段脚本。
    pub fn run(&mut self, cx: Context<'gc>, engine: &mut impl Engine, src: &Source<'_>) -> Result<Value<'gc>, Error> {
        engine.eval(self, cx, src)
    }
}
