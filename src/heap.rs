//! 堆：新生代（按块碰撞分配）、老年代（按尺寸档分页、页一次切成槽 + 空闲表）、大对象（单独分配）。
use alloc::alloc::{Layout, alloc, dealloc, handle_alloc_error};
use alloc::vec::Vec;
use core::ptr::NonNull;

use crate::header::{Flags, HEADER, Ptr, SizeClass, VTable};
use crate::parallel::{Pool, Spin, split};

/// 新生代一块的大小
pub(crate) const CHUNK: usize = 256 << 10;
/// 新生代用到这么多就做次回收
pub(crate) const NURSERY_LIMIT: usize = 4 << 20;
/// 老年代一页的大小（页按它对齐）
const PAGE: usize = 256 << 10;
/// 页里活着的不到这个比例（分母）就搬空它
const SPARSE: usize = 4;
/// 能腾出的空间不到这么多就不压缩
const COMPACT_MIN: usize = 1 << 20;
/// 晋升缓冲一次领几个槽
const LAB_SLOTS: usize = 64;
/// 空页留着复用：至少留这么多页（还给系统再要回来要 mmap、缺页）
const SPARE_MIN: usize = 64;
/// 空页最多留活页数的几分之一（分母）
const SPARE_RATIO: usize = 4;

/// 按 `layout` 向全局分配器要一块，失败就报分配错误
fn raw(layout: Layout) -> NonNull<u8> {
    // SAFETY: 大小都不为零
    NonNull::new(unsafe { alloc(layout) }).unwrap_or_else(|| handle_alloc_error(layout))
}

/// 新生代：一串块，当前块碰撞分配（游标在 `Context` 里）。
#[derive(Default)]
pub(crate) struct Nursery {
    /// 在用的块，最后一个是当前块
    chunks: Vec<NonNull<u8>>,
    /// 回收后留着复用的块
    spare: Vec<NonNull<u8>>,
    /// 要析构的新对象：次回收时没搬走的就是死了
    pub(crate) drops: Vec<Ptr>,
}

impl Nursery {
    const LAYOUT: Layout = match Layout::from_size_align(CHUNK, HEADER) {
        Ok(l) => l,
        Err(_) => panic!("chunk layout"),
    };

    /// 换一块新的，给出它的 `[起点, 终点)`
    pub(crate) fn refill(&mut self) -> (usize, usize) {
        let c = self.spare.pop().unwrap_or_else(|| raw(Self::LAYOUT));
        self.chunks.push(c);
        let start = c.as_ptr() as usize;
        (start, start + CHUNK)
    }

    /// 用了多少字节（`cursor` 是当前块的游标）
    pub(crate) fn used(&self, cursor: usize) -> usize {
        match self.chunks.last() {
            Some(c) => (self.chunks.len() - 1) * CHUNK + (cursor - c.as_ptr() as usize),
            None => 0,
        }
    }

    /// 全部清空：块留一部分复用
    pub(crate) fn reset(&mut self) {
        self.spare.append(&mut self.chunks);
        while self.spare.len() > NURSERY_LIMIT / CHUNK {
            let c = self.spare.pop().expect("spare chunk");
            // SAFETY: 按 LAYOUT 分配的
            unsafe { dealloc(c.as_ptr(), Self::LAYOUT) }
        }
    }
}

impl Drop for Nursery {
    fn drop(&mut self) {
        for c in self.chunks.drain(..).chain(self.spare.drain(..)) {
            // SAFETY: 按 LAYOUT 分配的
            unsafe { dealloc(c.as_ptr(), Self::LAYOUT) }
        }
    }
}

/// 老年代的一页：一个尺寸档的槽排满，空槽头上标 `FREE`。
struct Page {
    base: NonNull<u8>,
    class: usize,
    /// 上次清扫后活着的槽数
    live: usize,
    /// 压缩时要搬空
    evacuate: bool,
}

// SAFETY: 页是这块内存的唯一主人
unsafe impl Send for Page {}

impl Page {
    const LAYOUT: Layout = match Layout::from_size_align(PAGE, PAGE) {
        Ok(l) => l,
        Err(_) => panic!("page layout"),
    };

    fn slot(&self) -> usize {
        SizeClass::SLOTS[self.class]
    }

    /// 页里的各个槽（值指针）
    fn slots(&self) -> impl Iterator<Item = Ptr> + use<> {
        let (base, slot) = (self.base, self.slot());
        (0..PAGE / slot).map(move |i| {
            // SAFETY: i * slot < PAGE
            Ptr::at_slot(unsafe { base.add(i * slot) })
        })
    }

    fn contains(&self, p: Ptr) -> bool {
        p.addr() & !(PAGE - 1) == self.base.as_ptr() as usize
    }

    /// 清扫一页：没标记、不用析构的直接收回；要析构的交回调用线程。返回空槽与要析构的
    fn sweep(&mut self, marked: impl Fn(u32) -> bool) -> (Vec<Ptr>, Vec<Ptr>) {
        let (mut free, mut dead) = (Vec::new(), Vec::new());
        self.live = 0;
        for p in self.slots() {
            let h = p.header();
            let f = h.flags();
            if f & Flags::FREE != 0 {
                free.push(p);
            } else if marked(f) {
                self.live += 1;
            } else if p.vtable().drop.is_some() {
                dead.push(p);
            } else {
                h.free();
                free.push(p);
            }
        }
        (free, dead)
    }
}

/// 一个大对象。
struct Large {
    base: NonNull<u8>,
    layout: Layout,
    ptr: Ptr,
}

/// 老年代。
pub(crate) struct OldSpace {
    pages: Vec<Page>,
    /// 空出来留着复用的页
    spare: Vec<NonNull<u8>>,
    /// 各档的空槽
    free: [Vec<Ptr>; SizeClass::LARGE],
    large: Vec<Large>,
    /// 占用的字节（活槽 + 大对象）
    pub(crate) bytes: usize,
}

// SAFETY: 页与大对象的内存归它；回收时由自旋锁保护
unsafe impl Send for OldSpace {}

impl Default for OldSpace {
    fn default() -> Self {
        Self { pages: Vec::new(), spare: Vec::new(), free: core::array::from_fn(|_| Vec::new()), large: Vec::new(), bytes: 0 }
    }
}

impl OldSpace {
    /// 领至多 `n` 个 `class` 档的空槽放进 `out`（没有就开一页，先用留着的空页）
    pub(crate) fn take(&mut self, class: usize, n: usize, out: &mut Vec<Ptr>) {
        if self.free[class].is_empty() {
            let base = self.new_page(class);
            let free = &mut self.free[class];
            free.extend(Page { base, class, live: 0, evacuate: false }.slots().inspect(|p| p.header().free()));
        }
        let free = &mut self.free[class];
        out.extend(free.drain(free.len().saturating_sub(n)..));
    }

    /// 开一页（先用留着的空页）登记下来；槽的头由拿到它的一方写
    fn new_page(&mut self, class: usize) -> NonNull<u8> {
        let base = self.spare.pop().unwrap_or_else(|| raw(Page::LAYOUT));
        self.pages.push(Page { base, class, live: 0, evacuate: false });
        base
    }

    /// 给晋升缓冲：空闲表里有就领一批，没有就整页归它（返回页的 `[起点, 终点)`）
    fn refill(&mut self, class: usize, out: &mut Vec<Ptr>) -> Option<(usize, usize)> {
        if self.free[class].is_empty() {
            let start = self.new_page(class).as_ptr() as usize;
            let slot = SizeClass::SLOTS[class];
            return Some((start, start + PAGE / slot * slot));
        }
        self.take(class, LAB_SLOTS, out);
        None
    }

    /// 空页留着复用，多了才还给系统
    fn retire(&mut self, base: NonNull<u8>) {
        if self.spare.len() < SPARE_MIN.max(self.pages.len() / SPARE_RATIO) {
            self.spare.push(base);
        } else {
            // SAFETY: 按 Page::LAYOUT 分配的，里面没有活对象了
            unsafe { dealloc(base.as_ptr(), Page::LAYOUT) }
        }
    }

    /// 没用完的空槽还回来
    pub(crate) fn give(&mut self, class: usize, slots: &mut Vec<Ptr>) {
        self.free[class].append(slots);
    }

    /// 分一个大对象（头没写）
    pub(crate) fn alloc_large(&mut self, vt: &VTable) -> Ptr {
        let offset = vt.align.max(HEADER);
        let layout = Layout::from_size_align(offset + vt.size, offset).expect("large object layout");
        let base = raw(layout);
        // SAFETY: offset < layout.size()
        let ptr = Ptr::from(unsafe { base.add(offset) });
        self.bytes += layout.size();
        self.large.push(Large { base, layout, ptr });
        ptr
    }

    /// 清扫（各页并行）：没标记的收回，要析构的在调用线程上析构；空页还给分配器。返回活着的字节
    pub(crate) fn sweep(&mut self, pool: &dyn Pool, epoch: bool) -> usize {
        let marked = move |f: u32| (f & Flags::MARK != 0) == epoch;
        let swept = split(pool, &mut self.pages, &|pages: &mut [Page]| pages.iter_mut().map(|p| p.sweep(marked)).collect::<Vec<_>>());
        self.free.iter_mut().for_each(Vec::clear);
        let mut bytes = 0;
        let mut kept = Vec::with_capacity(self.pages.len());
        let pages = core::mem::take(&mut self.pages);
        for (page, (mut free, dead)) in pages.into_iter().zip(swept.into_iter().flatten()) {
            for p in dead {
                // SAFETY: 没标记 ⇒ 不可达，只析构这一次；值的类型未必能跨线程析构，所以在这里做
                unsafe { p.drop_value() };
                p.header().free();
                free.push(p);
            }
            if page.live == 0 {
                self.retire(page.base);
                continue;
            }
            bytes += page.live * page.slot();
            self.free[page.class].append(&mut free);
            kept.push(page);
        }
        self.pages = kept;
        self.large.retain(|l| {
            if marked(l.ptr.header().flags()) {
                bytes += l.layout.size();
                return true;
            }
            // SAFETY: 不可达，析构一次后还掉
            unsafe {
                l.ptr.drop_value();
                dealloc(l.base.as_ptr(), l.layout);
            }
            false
        });
        self.bytes = bytes;
        bytes
    }

    /// 选出要搬空的稀疏页（值得才选），并把它们的空槽从空闲表拿掉。返回要搬的对象及其尺寸档
    pub(crate) fn select_evacuation(&mut self) -> Vec<(Ptr, usize)> {
        let mut reclaim = 0;
        for p in &mut self.pages {
            if p.live * p.slot() * SPARSE < PAGE {
                p.evacuate = true;
                reclaim += PAGE - p.live * p.slot();
            }
        }
        if reclaim < COMPACT_MIN {
            self.pages.iter_mut().for_each(|p| p.evacuate = false);
            return Vec::new();
        }
        let pages = &self.pages;
        for free in &mut self.free {
            free.retain(|&s| !pages.iter().any(|g| g.evacuate && g.contains(s)));
        }
        pages.iter().filter(|p| p.evacuate).flat_map(|p| p.slots().map(move |s| (s, p.class))).filter(|(s, _)| s.header().flags() & Flags::FREE == 0).collect()
    }

    /// 还掉搬空了的页
    pub(crate) fn release_evacuated(&mut self, moved_bytes: usize) {
        let (gone, kept): (Vec<Page>, Vec<Page>) = core::mem::take(&mut self.pages).into_iter().partition(|p| p.evacuate);
        self.pages = kept;
        for p in gone {
            self.retire(p.base);
        }
        // 搬过去的新槽已经按晋升缓冲记过一次
        self.bytes -= moved_bytes;
    }

    /// 老年代里每个活着的对象（不含搬空中的页）
    pub(crate) fn live(&self) -> Vec<Ptr> {
        self.pages
            .iter()
            .filter(|p| !p.evacuate)
            .flat_map(Page::slots)
            .filter(|s| s.header().flags() & Flags::FREE == 0)
            .chain(self.large.iter().map(|l| l.ptr))
            .collect()
    }

    /// 调试：指针落在哪
    #[cfg(feature = "verify")]
    pub(crate) fn place(&self, p: Ptr) -> &'static str {
        if let Some(g) = self.pages.iter().find(|g| g.contains(p)) {
            return if g.evacuate { "evacuating page" } else { "page" };
        }
        if self.large.iter().any(|l| l.ptr == p) { "large" } else { "unknown" }
    }
}

impl Drop for OldSpace {
    fn drop(&mut self) {
        for p in self.live() {
            // SAFETY: 整个堆在拆，每个对象析构一次
            unsafe { p.drop_value() }
        }
        for base in self.pages.drain(..).map(|p| p.base).chain(self.spare.drain(..)) {
            // SAFETY: 按 Page::LAYOUT 分配的
            unsafe { dealloc(base.as_ptr(), Page::LAYOUT) }
        }
        for l in self.large.drain(..) {
            // SAFETY: 按 l.layout 分配的
            unsafe { dealloc(l.base.as_ptr(), l.layout) }
        }
    }
}

/// 晋升缓冲：一个工人往老年代放对象时用的空槽，一次从老年代领一批，省得每个都拿锁。
pub(crate) struct Lab<'a> {
    old: &'a Spin<OldSpace>,
    /// 各档从空闲表领来的槽
    slots: [Vec<Ptr>; SizeClass::LARGE],
    /// 各档独占的整页里还没用的 `[游标, 终点)`
    bump: [(usize, usize); SizeClass::LARGE],
    /// 用掉的槽的字节
    pub(crate) bytes: usize,
}

impl<'a> Lab<'a> {
    pub(crate) fn new(old: &'a Spin<OldSpace>) -> Self {
        Self { old, slots: core::array::from_fn(|_| Vec::new()), bump: [(0, 0); SizeClass::LARGE], bytes: 0 }
    }

    /// 一个 `class` 档的空槽（头由调用方写）
    pub(crate) fn slot(&mut self, class: usize) -> Ptr {
        let size = SizeClass::SLOTS[class];
        self.bytes += size;
        loop {
            if let Some(p) = self.slots[class].pop() {
                return p;
            }
            let (at, end) = self.bump[class];
            if at + size <= end {
                self.bump[class].0 = at + size;
                return Ptr::at_slot(NonNull::new(at as *mut u8).expect("page slot"));
            }
            if let Some(page) = self.old.lock().refill(class, &mut self.slots[class]) {
                self.bump[class] = page;
            }
        }
    }
}

impl Drop for Lab<'_> {
    /// 独占页里没用的槽标成空槽，连同没用完的一起还回去；用掉的记进老年代
    fn drop(&mut self) {
        for (class, &(at, end)) in self.bump.iter().enumerate() {
            let size = SizeClass::SLOTS[class];
            self.slots[class].extend((at..end).step_by(size).map(|a| Ptr::at_slot(NonNull::new(a as *mut u8).expect("page slot"))).inspect(|p| p.header().free()));
        }
        let mut old = self.old.lock();
        for (class, slots) in self.slots.iter_mut().enumerate() {
            old.give(class, slots);
        }
        old.bytes += self.bytes;
    }
}
