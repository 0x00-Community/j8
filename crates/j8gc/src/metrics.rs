//! Collection counters.
use core::sync::atomic::{AtomicUsize, Ordering};

/// How many times each kind of collection has run.
#[derive(Default, Debug)]
pub struct Metrics {
    pub(crate) minor: AtomicUsize,
    pub(crate) major: AtomicUsize,
    pub(crate) compactions: AtomicUsize,
    pub(crate) promoted: AtomicUsize,
}

impl Metrics {
    /// Number of minor (young-generation) collections
    pub fn minor_count(&self) -> usize {
        self.minor.load(Ordering::Relaxed)
    }

    /// Number of major (whole-heap mark-sweep) collections
    pub fn major_count(&self) -> usize {
        self.major.load(Ordering::Relaxed)
    }

    /// Number of compactions
    pub fn compaction_count(&self) -> usize {
        self.compactions.load(Ordering::Relaxed)
    }

    /// Number of objects promoted from the young generation to the old generation
    pub fn promoted_count(&self) -> usize {
        self.promoted.load(Ordering::Relaxed)
    }

    /// Number of full collection cycles (same as the number of major collections)
    pub fn total_gc_count(&self) -> usize {
        self.major.load(Ordering::Relaxed)
    }
}
