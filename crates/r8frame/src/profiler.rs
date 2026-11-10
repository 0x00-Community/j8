use alloc::collections::BTreeMap;

/// 一个 JS 函数在 isolate 里的编号（各执行层共用）。
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct FuncId(pub u32);

/// 一个函数的热度。
#[derive(Clone, Copy, Debug, Default)]
pub struct Counter {
    pub calls: u32,
    pub backedges: u32,
}

/// 调用 / 回边计数；关着时什么都不记。
#[derive(Debug, Default)]
pub struct Profiler {
    enabled: bool,
    counters: BTreeMap<FuncId, Counter>,
}

impl Profiler {
    pub fn enable(&mut self, on: bool) {
        self.enabled = on;
    }

    pub fn enter(&mut self, f: FuncId) {
        if self.enabled {
            self.counters.entry(f).or_default().calls += 1;
        }
    }

    pub fn backedge(&mut self, f: FuncId) {
        if self.enabled {
            self.counters.entry(f).or_default().backedges += 1;
        }
    }

    pub fn counter(&self, f: FuncId) -> Counter {
        self.counters.get(&f).copied().unwrap_or_default()
    }

    /// 清掉一个函数的计数（升层后重新计）。
    pub fn reset(&mut self, f: FuncId) {
        self.counters.remove(&f);
    }
}
