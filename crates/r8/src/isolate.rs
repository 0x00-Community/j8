use crate::{Context, Heap, HandleScope, Object, Platform, Profiler, Value};

/// 建 isolate 的参数。
#[derive(Clone, Copy, Debug, Default)]
pub struct CreateParams {
    /// 开不开剖析器
    pub profile: bool,
}

/// 一个独立的 JS 世界：堆、句柄表、剖析器；平台由嵌入方借给它。
pub struct Isolate<'p> {
    platform: &'p dyn Platform,
    heap: Heap,
    pub(crate) locals: Vec<Value>,
    pub(crate) globals: Vec<Option<Value>>,
    profiler: Profiler,
}

impl<'p> Isolate<'p> {
    pub fn new(platform: &'p dyn Platform, params: CreateParams) -> Self {
        let mut profiler = Profiler::default();
        profiler.enable(params.profile);
        Self { platform, heap: Heap::default(), locals: Vec::new(), globals: Vec::new(), profiler }
    }

    pub fn platform(&self) -> &'p dyn Platform {
        self.platform
    }

    pub fn heap(&self) -> &Heap {
        &self.heap
    }

    pub fn heap_mut(&mut self) -> &mut Heap {
        &mut self.heap
    }

    pub fn profiler(&self) -> &Profiler {
        &self.profiler
    }

    pub fn profiler_mut(&mut self) -> &mut Profiler {
        &mut self.profiler
    }

    /// 开一个句柄作用域，作用域结束时其中的 [`Local`](crate::Local) 全部失效。
    pub fn scope(&mut self) -> HandleScope<'_, 'p> {
        HandleScope::new(self)
    }

    /// 新建一个带全新全局对象的执行环境。
    pub fn context(&mut self) -> Context<'_, 'p> {
        let global = self.heap.alloc(Object::default());
        Context::new(self, global)
    }
}
