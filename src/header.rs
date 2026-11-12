//! 对象头与类型表：每个对象前面紧挨着 16 字节的头。头是原子的：并行回收时多个线程同时认领、转发。
use core::hint::spin_loop;
use core::mem::{align_of, needs_drop, size_of};
use core::ptr::NonNull;
use core::sync::atomic::{AtomicU32, AtomicUsize, Ordering};

use crate::{Collect, Relocate, Trace};

/// 对象头的大小，也是对象的对齐上限（更大的对齐走大对象）
pub(crate) const HEADER: usize = size_of::<Header>();

/// 头里的标志位。
pub(crate) struct Flags;

impl Flags {
    /// 在老年代（页或大对象）；没有就是在新生代
    pub(crate) const OLD: u32 = 1;
    /// 标记位：与当前纪元相同才算标记过（每轮主回收翻一次纪元，不用逐个清）
    pub(crate) const MARK: u32 = 1 << 1;
    /// 在某个灰色栈里（已认领、还没遍历）
    pub(crate) const GRAY: u32 = 1 << 2;
    /// 老对象在记忆集里（可能指向新生代）
    pub(crate) const REMEMBERED: u32 = 1 << 3;
    /// 已搬走：`meta` 是新地址
    pub(crate) const FORWARDED: u32 = 1 << 4;
    /// 大对象（单独分配，不搬）
    pub(crate) const LARGE: u32 = 1 << 5;
    /// 老年代页里的空槽
    pub(crate) const FREE: u32 = 1 << 6;
    /// 正被某个线程搬：别的线程等它装好转发地址
    pub(crate) const BUSY: u32 = 1 << 7;
}

/// 对象头。
#[repr(C, align(16))]
pub(crate) struct Header {
    /// 类型表地址；搬走后是新对象的地址
    meta: AtomicUsize,
    flags: AtomicU32,
}

/// 搬一个新生代对象前认领它的结果。
pub(crate) enum Claim {
    /// 本来就在老年代
    Old,
    /// 已经搬走了
    Forwarded(Ptr),
    /// 归我搬
    Mine,
}

impl Header {
    pub(crate) fn new(vtable: &VTable, flags: u32) -> Self {
        Self { meta: AtomicUsize::new(core::ptr::from_ref(vtable) as usize), flags: AtomicU32::new(flags) }
    }

    #[inline]
    pub(crate) fn flags(&self) -> u32 {
        self.flags.load(Ordering::Relaxed)
    }

    #[inline]
    pub(crate) fn or_flags(&self, f: u32) {
        self.flags.fetch_or(f, Ordering::Relaxed);
    }

    #[inline]
    pub(crate) fn and_flags(&self, f: u32) {
        self.flags.fetch_and(f, Ordering::Relaxed);
    }

    /// 按 `f` 改标志（返回 `None` 就不改，CAS 失败重来）；返回是否改成了
    #[inline]
    pub(crate) fn update(&self, f: impl FnMut(u32) -> Option<u32>) -> bool {
        self.flags.fetch_update(Ordering::AcqRel, Ordering::Acquire, f).is_ok()
    }

    /// 搬之前认领：别人正在搬就等它搬完
    pub(crate) fn claim(&self) -> Claim {
        loop {
            let f = self.flags.load(Ordering::Acquire);
            if f & Flags::OLD != 0 {
                return Claim::Old;
            }
            if f & Flags::FORWARDED != 0 {
                return Claim::Forwarded(Ptr::from(self.meta.load(Ordering::Acquire)));
            }
            if f & Flags::BUSY != 0 {
                spin_loop();
                continue;
            }
            if self.flags.compare_exchange_weak(f, f | Flags::BUSY, Ordering::AcqRel, Ordering::Relaxed).is_ok() {
                return Claim::Mine;
            }
        }
    }

    /// 搬完：头改记新地址
    pub(crate) fn forward(&self, to: Ptr) {
        self.meta.store(to.addr(), Ordering::Release);
        self.flags.store(Flags::FORWARDED, Ordering::Release);
    }

    /// 搬到了哪
    pub(crate) fn forwarded(&self) -> Option<Ptr> {
        (self.flags.load(Ordering::Acquire) & Flags::FORWARDED != 0).then(|| Ptr::from(self.meta.load(Ordering::Acquire)))
    }

    /// 标成空槽
    pub(crate) fn free(&self) {
        self.meta.store(0, Ordering::Relaxed);
        self.flags.store(Flags::FREE, Ordering::Relaxed);
    }
}

/// 一种类型的回收操作。按类型常量提升出来，头里只存它的地址。
pub(crate) struct VTable {
    /// 值的大小与对齐
    pub(crate) size: usize,
    pub(crate) align: usize,
    /// 头 + 值，按 16 字节取整
    pub(crate) slot: usize,
    /// 老年代的尺寸档；`SizeClass::LARGE` 是大对象
    pub(crate) class: usize,
    pub(crate) needs_trace: bool,
    pub(crate) trace: unsafe fn(Ptr, &mut Trace<'_>),
    pub(crate) relocate: unsafe fn(Ptr, &mut Relocate<'_>),
    pub(crate) drop: Option<unsafe fn(Ptr)>,
}

impl VTable {
    const fn of<'gc, T: Collect<'gc>>() -> Self {
        let slot = (HEADER + size_of::<T>()).next_multiple_of(HEADER);
        Self {
            size: size_of::<T>(),
            align: align_of::<T>(),
            slot,
            class: if align_of::<T>() > HEADER { SizeClass::LARGE } else { SizeClass::of(slot) },
            needs_trace: T::NEEDS_TRACE,
            trace: Ptr::trace_as::<T>,
            relocate: Ptr::relocate_as::<T>,
            drop: if needs_drop::<T>() { Some(Ptr::drop_as::<T>) } else { None },
        }
    }
}

/// 每种类型一张类型表（常量，取地址时提升成只读数据）。
pub(crate) trait Typed<'gc>: Collect<'gc> + Sized {
    const VTABLE: VTable = VTable::of::<Self>();
}

impl<'gc, T: Collect<'gc>> Typed<'gc> for T {}

/// 老年代的尺寸档：页里的槽一样大。
pub(crate) struct SizeClass;

impl SizeClass {
    /// 各档的槽大小（含头）
    pub(crate) const SLOTS: [usize; 32] = [
        16, 32, 48, 64, 80, 96, 112, 128, 160, 192, 224, 256, 320, 384, 448, 512, 640, 768, 896, 1024, 1280, 1536, 1792, 2048, 2560, 3072, 3584, 4096, 5120,
        6144, 7168, 8192,
    ];
    /// 超过最大档：大对象
    pub(crate) const LARGE: usize = Self::SLOTS.len();

    const fn of(slot: usize) -> usize {
        let mut i = 0;
        while i < Self::SLOTS.len() {
            if Self::SLOTS[i] >= slot {
                return i;
            }
            i += 1;
        }
        Self::LARGE
    }
}

/// 擦除了类型的对象指针：指向值（头在它前面 [`HEADER`] 字节）。
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(crate) struct Ptr(NonNull<u8>);

// SAFETY: 只是地址。并行回收时 mutator 停着，每个对象同一时刻只归一个线程（头上的原子位认领）
unsafe impl Send for Ptr {}
unsafe impl Sync for Ptr {}

impl From<usize> for Ptr {
    fn from(addr: usize) -> Self {
        Self(NonNull::new(addr as *mut u8).expect("null gc pointer"))
    }
}

impl<T> From<NonNull<T>> for Ptr {
    fn from(p: NonNull<T>) -> Self {
        Self(p.cast())
    }
}

impl Ptr {
    /// 槽的起点（头所在）⇒ 值
    pub(crate) fn at_slot(base: NonNull<u8>) -> Self {
        // SAFETY: 槽至少有一个头大
        Self(unsafe { base.add(HEADER) })
    }

    #[inline]
    pub(crate) fn addr(self) -> usize {
        self.0.as_ptr() as usize
    }

    #[inline]
    pub(crate) fn as_ptr(self) -> *mut u8 {
        self.0.as_ptr()
    }

    pub(crate) fn cast<T>(self) -> NonNull<T> {
        self.0.cast()
    }

    #[inline]
    pub(crate) fn header<'a>(self) -> &'a Header {
        // SAFETY: 每个对象前面都有头，对象活着头就活着
        unsafe { self.0.sub(HEADER).cast::<Header>().as_ref() }
    }

    /// 没搬走的对象的类型表
    #[inline]
    pub(crate) fn vtable<'a>(self) -> &'a VTable {
        debug_assert!(self.header().flags() & (Flags::FORWARDED | Flags::FREE) == 0, "vtable of a moved or freed object {self:?}");
        // SAFETY: 没搬走时 meta 是 `VTable` 常量的地址
        unsafe { &*(self.header().meta.load(Ordering::Relaxed) as *const VTable) }
    }

    /// 写对象头
    ///
    /// # Safety
    /// 前面 [`HEADER`] 字节是这个槽自己的。
    pub(crate) unsafe fn init(self, vt: &VTable, flags: u32) {
        unsafe { self.0.sub(HEADER).cast::<Header>().write(Header::new(vt, flags)) };
    }

    /// 把值复制到空槽 `to` 并写好它的头（值按字节搬：Rust 值都能按位移动）
    ///
    /// # Safety
    /// `to` 是同尺寸档、归调用方的空槽；`vt` 是这个对象的类型表。
    pub(crate) unsafe fn copy_to(self, vt: &VTable, to: Ptr, flags: u32) {
        unsafe {
            core::ptr::copy_nonoverlapping(self.as_ptr(), to.as_ptr(), vt.size);
            to.init(vt, flags);
        }
    }

    unsafe fn trace_as<'gc, T: Collect<'gc>>(p: Ptr, t: &mut Trace<'_>) {
        unsafe { p.cast::<T>().as_ref() }.trace(t);
    }

    unsafe fn relocate_as<'gc, T: Collect<'gc>>(p: Ptr, r: &mut Relocate<'_>) {
        unsafe { p.cast::<T>().as_mut() }.relocate(r);
    }

    unsafe fn drop_as<'gc, T: Collect<'gc>>(p: Ptr) {
        unsafe { p.cast::<T>().drop_in_place() }
    }

    /// # Safety
    /// 对象活着、没搬走，且此刻只有当前线程在动它。
    pub(crate) unsafe fn trace(self, t: &mut Trace<'_>) {
        unsafe { (self.vtable().trace)(self, t) }
    }

    /// # Safety
    /// 同 [`Ptr::trace`]。
    pub(crate) unsafe fn relocate(self, r: &mut Relocate<'_>) {
        unsafe { (self.vtable().relocate)(self, r) }
    }

    /// # Safety
    /// 对象死了、没搬走、只析构一次。
    pub(crate) unsafe fn drop_value(self) {
        if let Some(d) = self.vtable().drop {
            unsafe { d(self) }
        }
    }
}
