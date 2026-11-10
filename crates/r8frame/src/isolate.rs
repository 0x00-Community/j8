use gc_arena::{Arena, Collect, DynamicRootSet, Mutation, Rootable};

use crate::{Platform, Profiler};

/// 建 isolate 的参数。
#[derive(Clone, Copy, Debug, Default)]
pub struct CreateParams {
    /// 开不开剖析器
    pub profile: bool,
}

/// GC 堆的根：持久句柄都挂在这里。
#[derive(Collect)]
#[collect(no_drop)]
struct Roots<'gc> {
    set: DynamicRootSet<'gc>,
}

/// 一个独立的 JS 世界：GC 堆加剖析器；平台由嵌入方借给它。
pub struct Isolate<'p> {
    platform: &'p dyn Platform,
    arena: Arena<Rootable![Roots<'_>]>,
    profiler: Profiler,
}

/// 一次进入 isolate：`'gc` 是这次进入的品牌，其中的值出不了闭包。
pub struct Scope<'gc, 'i> {
    pub mc: &'gc Mutation<'gc>,
    pub(crate) roots: &'gc DynamicRootSet<'gc>,
    pub platform: &'i dyn Platform,
    pub profiler: &'i mut Profiler,
}

impl<'p> Isolate<'p> {
    pub fn new(platform: &'p dyn Platform, params: CreateParams) -> Self {
        let mut profiler = Profiler::default();
        profiler.enable(params.profile);
        let arena = Arena::new(|mc| Roots { set: DynamicRootSet::new(mc) });
        Self { platform, arena, profiler }
    }

    /// 完成过几轮回收。
    pub fn gc_cycles(&self) -> usize {
        self.arena.metrics().total_gc_count()
    }

    /// 在执行环境 `cx` 里用 `engine` 跑一段脚本。
    pub fn run(&mut self, engine: &mut impl crate::Engine, cx: &crate::ContextRoot, src: &crate::Source<'_>) -> Result<crate::Global, crate::Error> {
        engine.eval(self, cx, src)
    }

    pub fn profiler(&self) -> &Profiler {
        &self.profiler
    }

    /// 进入 isolate 跑 `f`；`f` 期间不回收，返回后按欠账做一段增量回收。
    pub fn enter<R>(&mut self, f: impl for<'gc> FnOnce(&mut Scope<'gc, '_>) -> R) -> R {
        let (platform, profiler) = (self.platform, &mut self.profiler);
        let r = self.arena.mutate(|mc, roots| f(&mut Scope { mc, roots: &roots.set, platform, profiler }));
        self.arena.collect_debt();
        r
    }
}
