use alloc::collections::BTreeMap;

use crate::{Collect};

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord, Collect)]
#[collect(require_static)]
pub struct FuncId(pub u32);

#[derive(Clone, Copy, Debug, Default)]
pub struct Counter {
    pub calls: u32,
    pub backedges: u32,
}

#[derive(Debug, Default)]
pub struct Profiler {
    enabled: bool,
    counters: BTreeMap<FuncId, Counter>,
}

impl Profiler {
    pub fn enable(&mut self, on: bool) {
        self.enabled = on;
    }

    #[inline]
    pub fn enter(&mut self, f: FuncId) {
        if self.enabled {
            self.counter_mut(f).calls += 1;
        }
    }

    #[inline]
    pub fn backedge(&mut self, f: FuncId) {
        if self.enabled {
            self.counter_mut(f).backedges += 1;
        }
    }

    #[inline(never)]
    fn counter_mut(&mut self, f: FuncId) -> &mut Counter {
        self.counters.entry(f).or_default()
    }

    pub fn counter(&self, f: FuncId) -> Counter {
        self.counters.get(&f).copied().unwrap_or_default()
    }

    pub fn reset(&mut self, f: FuncId) {
        self.counters.remove(&f);
    }
}
