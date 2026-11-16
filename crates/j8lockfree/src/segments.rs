//! 分段槽表：位置 ⇒ 槽，第 k 段有 `BASE << k` 个槽；段表定长（段长翻倍，`usize::BITS` 段覆盖全部位置）。
//! 段按需 CAS 装上，并发期间只装不拆，独占（`&mut`）时才释放：没有回收竞争。
use alloc::boxed::Box;
use core::mem;
use core::ptr;
use core::sync::atomic::{AtomicPtr, Ordering};

/// 第 0 段的槽数
const BASE: usize = 32;
/// 段表长度
const SEGMENTS: usize = usize::BITS as usize;

pub(crate) struct Segments<S> {
    /// 第 k 项指向第 k 段的首槽，没装是空指针
    table: [AtomicPtr<S>; SEGMENTS],
}

impl<S> Default for Segments<S> {
    fn default() -> Self {
        Self { table: [const { AtomicPtr::new(ptr::null_mut()) }; SEGMENTS] }
    }
}

impl<S: Default> Segments<S> {
    /// 位置 ⇒（段号，段内下标）：第 k 段从位置 `BASE·(2^k − 1)` 开始
    fn locate(pos: usize) -> (usize, usize) {
        let k = (pos / BASE + 1).ilog2() as usize;
        (k, pos - BASE * ((1 << k) - 1))
    }

    /// 第 k 段的首槽；没装就装上（并发装时输家释放自己那份）
    fn segment(&self, k: usize) -> *mut S {
        let cur = self.table[k].load(Ordering::Acquire);
        if !cur.is_null() {
            return cur;
        }
        let fresh: Box<[S]> = (0..BASE << k).map(|_| S::default()).collect();
        let fresh = Box::into_raw(fresh).cast::<S>();
        match self.table[k].compare_exchange(ptr::null_mut(), fresh, Ordering::AcqRel, Ordering::Acquire) {
            Ok(_) => fresh,
            Err(won) => {
                // SAFETY: fresh 是刚才 Box::into_raw 出来、长 BASE << k、没给任何人看过的段
                drop(unsafe { Box::from_raw(ptr::slice_from_raw_parts_mut(fresh, BASE << k)) });
                won
            }
        }
    }

    /// 位置 `pos` 的槽（所在段没装就装上）
    pub(crate) fn slot(&self, pos: usize) -> &S {
        let (k, i) = Self::locate(pos);
        // SAFETY: 段装上后在独占释放前一直有效，i < BASE << k
        unsafe { &*self.segment(k).add(i) }
    }

    /// 独占时位置 `pos` 的槽；所在段没装返回 None
    pub(crate) fn slot_mut(&mut self, pos: usize) -> Option<&mut S> {
        let (k, i) = Self::locate(pos);
        let p = *self.table[k].get_mut();
        // SAFETY: 独占；段长 BASE << k > i
        (!p.is_null()).then(|| unsafe { &mut *p.add(i) })
    }

    /// 释放所有段（槽按 `S` 的 Drop 析构）
    pub(crate) fn release(&mut self) {
        for (k, seg) in self.table.iter_mut().enumerate() {
            let p = mem::replace(seg.get_mut(), ptr::null_mut());
            if !p.is_null() {
                // SAFETY: 独占；p 是 segment(k) 装上的长 BASE << k 的段
                drop(unsafe { Box::from_raw(ptr::slice_from_raw_parts_mut(p, BASE << k)) });
            }
        }
    }
}

impl<S> Drop for Segments<S> {
    fn drop(&mut self) {
        for (k, seg) in self.table.iter_mut().enumerate() {
            let p = *seg.get_mut();
            if !p.is_null() {
                // SAFETY: 同 release
                drop(unsafe { Box::from_raw(ptr::slice_from_raw_parts_mut(p, BASE << k)) });
            }
        }
    }
}
