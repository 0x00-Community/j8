//! `Collect`：回收器怎么遍历一个值里的 `Gc`。
use alloc::boxed::Box;
use alloc::collections::{BTreeMap, BTreeSet, VecDeque};
use alloc::rc::Rc;
use alloc::string::String;
use alloc::vec::Vec;
use core::cell::{Cell, RefCell};
use core::marker::PhantomData;

use crate::header::Ptr;

/// 能放进 `Gc` 的类型。
///
/// # Safety
/// - `trace` 必须访问值里的每一个 `Gc`，`relocate` 必须改写每一个 `Gc`（对象会被搬走）
/// - `trace` / `relocate` 只碰这个值自己：回收是并行的，不同对象在不同线程上同时遍历
/// - `Drop` 里不得碰 `Gc`（析构时它们可能已经没了）；派生宏用 `no_drop` 保证
/// - 借内部可变性收进新的 `Gc` 必须经写屏障（`Lock` / `RefLock` 已做好）
///
/// 一般用 `#[derive(Collect)]`，不手写。
pub unsafe trait Collect<'gc> {
    /// 值里不可能有 `Gc` 时设 `false`，回收器整个跳过它
    const NEEDS_TRACE: bool = true;

    /// 对每个 `Gc` 调 [`Trace::trace`]
    #[inline]
    fn trace(&self, _t: &mut Trace<'_>) {}

    /// 对每个 `Gc` 调 [`Relocate::relocate`]（字段原地改成搬后的地址）
    #[inline]
    fn relocate(&mut self, _r: &mut Relocate<'_>) {}
}

/// 标记时看到一条边的工人（标记 / 调试校验）。
pub(crate) trait Visit {
    fn visit(&mut self, p: Ptr);
}

/// 搬对象时改一个字段的工人（晋升 / 压缩）。
pub(crate) trait Mover {
    fn forward(&mut self, p: Ptr) -> Ptr;
}

/// 标记时的访问器（每个回收线程各一个）。
pub struct Trace<'a> {
    visit: &'a mut (dyn Visit + 'a),
}

impl<'a> Trace<'a> {
    pub(crate) fn new(visit: &'a mut (dyn Visit + 'a)) -> Self {
        Self { visit }
    }

    #[inline]
    pub fn trace<'gc, C: Collect<'gc> + ?Sized>(&mut self, value: &C) {
        if C::NEEDS_TRACE {
            value.trace(self);
        }
    }

    #[inline]
    pub(crate) fn ptr(&mut self, p: Ptr) {
        self.visit.visit(p);
    }
}

/// 搬对象时的访问器：把字段改成新地址（每个回收线程各一个）。
pub struct Relocate<'a> {
    mover: &'a mut (dyn Mover + 'a),
    moves: usize,
}

impl<'a> Relocate<'a> {
    pub(crate) fn new(mover: &'a mut (dyn Mover + 'a)) -> Self {
        Self { mover, moves: 0 }
    }

    #[inline]
    pub fn relocate<'gc, C: Collect<'gc> + ?Sized>(&mut self, value: &mut C) {
        if C::NEEDS_TRACE {
            value.relocate(self);
        }
    }

    /// 到目前为止改过几个指针。按地址做哈希的容器拿它判断要不要重建索引。
    pub fn moves(&self) -> usize {
        self.moves
    }

    #[inline]
    pub(crate) fn ptr(&mut self, p: Ptr) -> Ptr {
        let q = self.mover.forward(p);
        if q != p {
            self.moves += 1;
        }
        q
    }
}

/// 不含 `Gc` 的类型：什么都不用做
macro_rules! leaf {
    ($($ty:ty),* $(,)?) => {$(
        unsafe impl<'gc> Collect<'gc> for $ty {
            const NEEDS_TRACE: bool = false;
        }
    )*};
}
leaf!((), bool, char, u8, u16, u32, u64, u128, usize, i8, i16, i32, i64, i128, isize, f32, f64, str, String);

/// `'static` 的包装：里面不可能有 `Gc`
macro_rules! leaf_static {
    ($($ty:ident),*) => {$(
        unsafe impl<'gc, T: ?Sized + 'static> Collect<'gc> for $ty<T> {
            const NEEDS_TRACE: bool = false;
        }
    )*};
}
leaf_static!(Rc, Cell, RefCell);

unsafe impl<T: ?Sized> Collect<'_> for PhantomData<T> {
    const NEEDS_TRACE: bool = false;
}

unsafe impl<T: ?Sized + 'static> Collect<'_> for &'static T {
    const NEEDS_TRACE: bool = false;
}

/// 逐个元素遍历的容器
macro_rules! sequence {
    ($($ty:ty),*) => {$(
        unsafe impl<'gc, T: Collect<'gc>> Collect<'gc> for $ty {
            const NEEDS_TRACE: bool = T::NEEDS_TRACE;

            #[inline]
            fn trace(&self, t: &mut Trace<'_>) {
                for v in self.iter() { t.trace(v); }
            }

            #[inline]
            fn relocate(&mut self, r: &mut Relocate<'_>) {
                for v in self.iter_mut() { r.relocate(v); }
            }
        }
    )*};
}
sequence!(Option<T>, Vec<T>, VecDeque<T>, [T]);

unsafe impl<'gc, T: Collect<'gc>, const N: usize> Collect<'gc> for [T; N] {
    const NEEDS_TRACE: bool = T::NEEDS_TRACE;

    fn trace(&self, t: &mut Trace<'_>) {
        for v in self {
            t.trace(v);
        }
    }

    fn relocate(&mut self, r: &mut Relocate<'_>) {
        for v in self.iter_mut() {
            r.relocate(v);
        }
    }
}

unsafe impl<'gc, T: Collect<'gc> + ?Sized> Collect<'gc> for Box<T> {
    const NEEDS_TRACE: bool = T::NEEDS_TRACE;

    fn trace(&self, t: &mut Trace<'_>) {
        t.trace(&**self);
    }

    fn relocate(&mut self, r: &mut Relocate<'_>) {
        r.relocate(&mut **self);
    }
}

unsafe impl<'gc, T: Collect<'gc>, E: Collect<'gc>> Collect<'gc> for Result<T, E> {
    const NEEDS_TRACE: bool = T::NEEDS_TRACE || E::NEEDS_TRACE;

    fn trace(&self, t: &mut Trace<'_>) {
        match self {
            Ok(v) => t.trace(v),
            Err(e) => t.trace(e),
        }
    }

    fn relocate(&mut self, r: &mut Relocate<'_>) {
        match self {
            Ok(v) => r.relocate(v),
            Err(e) => r.relocate(e),
        }
    }
}

/// 键不能含 `Gc`（键改了要重排，原地改不了），只遍历值
unsafe impl<'gc, K: Ord + 'static, V: Collect<'gc>> Collect<'gc> for BTreeMap<K, V> {
    const NEEDS_TRACE: bool = V::NEEDS_TRACE;

    fn trace(&self, t: &mut Trace<'_>) {
        for v in self.values() {
            t.trace(v);
        }
    }

    fn relocate(&mut self, r: &mut Relocate<'_>) {
        for v in self.values_mut() {
            r.relocate(v);
        }
    }
}

unsafe impl<T: Ord + 'static> Collect<'_> for BTreeSet<T> {
    const NEEDS_TRACE: bool = false;
}

/// 元组
macro_rules! tuple {
    ($(($($n:tt $t:ident),+)),*) => {$(
        unsafe impl<'gc, $($t: Collect<'gc>),+> Collect<'gc> for ($($t,)+) {
            const NEEDS_TRACE: bool = false $(|| $t::NEEDS_TRACE)+;

            fn trace(&self, t: &mut Trace<'_>) {
                $(t.trace(&self.$n);)+
            }

            fn relocate(&mut self, r: &mut Relocate<'_>) {
                $(r.relocate(&mut self.$n);)+
            }
        }
    )*};
}
tuple!((0 A), (0 A, 1 B), (0 A, 1 B, 2 C), (0 A, 1 B, 2 C, 3 D), (0 A, 1 B, 2 C, 3 D, 4 E), (0 A, 1 B, 2 C, 3 D, 4 E, 5 F));

/// 派生宏用：实现了 `Drop` 的类型会和它冲突（`#[collect(no_drop)]`）
#[doc(hidden)]
pub trait __MustNotImplDrop {}

#[allow(drop_bounds)]
impl<T: Drop> __MustNotImplDrop for T {}
