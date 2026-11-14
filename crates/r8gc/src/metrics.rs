//! 回收计数。
use core::cell::Cell;

/// 各种回收做过几次。
#[derive(Default, Debug)]
pub struct Metrics {
    pub(crate) minor: Cell<usize>,
    pub(crate) major: Cell<usize>,
    pub(crate) compactions: Cell<usize>,
    pub(crate) promoted: Cell<usize>,
}

impl Metrics {
    /// 次回收（新生代）次数
    pub fn minor_count(&self) -> usize {
        self.minor.get()
    }

    /// 主回收（整堆标记清扫）次数
    pub fn major_count(&self) -> usize {
        self.major.get()
    }

    /// 压缩次数
    pub fn compaction_count(&self) -> usize {
        self.compactions.get()
    }

    /// 从新生代提升进老年代的对象数
    pub fn promoted_count(&self) -> usize {
        self.promoted.get()
    }

    /// 完整回收的轮数（同主回收次数）
    pub fn total_gc_count(&self) -> usize {
        self.major.get()
    }
}
