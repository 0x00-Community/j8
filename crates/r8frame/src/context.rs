use crate::{Error, Engine, Isolate, ObjId, Source, Value};

/// 执行环境：借着 isolate，带一个全局对象。
pub struct Context<'i, 'p> {
    isolate: &'i mut Isolate<'p>,
    global: ObjId,
}

impl<'i, 'p> Context<'i, 'p> {
    pub(crate) fn new(isolate: &'i mut Isolate<'p>, global: ObjId) -> Self {
        Self { isolate, global }
    }

    pub fn isolate(&mut self) -> &mut Isolate<'p> {
        self.isolate
    }

    pub fn global(&self) -> ObjId {
        self.global
    }

    /// 在这个环境里用 `engine` 跑一段脚本。
    pub fn run(&mut self, engine: &mut impl Engine, src: &Source<'_>) -> Result<Value, Error> {
        engine.eval(self, src)
    }
}
