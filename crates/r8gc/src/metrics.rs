//! Collection counters.
use core::cell::Cell;

/// How many times each kind of collection has run.
#[derive(Default, Debug)]
pub struct Metrics {
    pub(crate) minor: Cell<usize>,
    pub(crate) major: Cell<usize>,
    pub(crate) compactions: Cell<usize>,
    pub(crate) promoted: Cell<usize>,
}

impl Metrics {
    /// Number of minor (young-generation) collections
    pub fn minor_count(&self) -> usize {
        self.minor.get()
    }

    /// Number of major (whole-heap mark-sweep) collections
    pub fn major_count(&self) -> usize {
        self.major.get()
    }

    /// Number of compactions
    pub fn compaction_count(&self) -> usize {
        self.compactions.get()
    }

    /// Number of objects promoted from the young generation to the old generation
    pub fn promoted_count(&self) -> usize {
        self.promoted.get()
    }

    /// Number of full collection cycles (same as the number of major collections)
    pub fn total_gc_count(&self) -> usize {
        self.major.get()
    }
}
