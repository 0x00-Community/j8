//! r8alloc：r8 的堆内存分配器，C11 写成（`src/r8alloc.c`），经 C ABI 接入。
//!
//! - 第一次使用时预留一段地址区（`mmap`，`MAP_NORESERVE`：首次触碰才提交物理内存）
//! - [`R8Alloc::pages`]：按页（[`R8Alloc::PAGE`] 字节、按页对齐）分配，释放的连续页按页数进各自的无锁空闲表、
//!   内存留着（复用不再缺页）；[`R8Alloc::trim`] 把空闲页的内存统一还给系统、地址留着；过长的连续页与地址区用完之后
//!   直接向系统要
//! - [`R8Alloc`] 实现 [`GlobalAlloc`]：小块按尺寸分档，每档一个全局无锁空闲表；更大的按页
//! - 没有按线程划分的 arena，也没有锁：任何线程都能释放别的线程分配的内存
//!
//! 空闲表是 Treiber 栈，表头一个 64 位字：32 位版本号 + 32 位链接，CAS 不会遇到 ABA。
#![no_std]

use core::alloc::{GlobalAlloc, Layout};
use core::ptr::NonNull;

unsafe extern "C" {
    fn r8a_alloc(size: usize, align: usize) -> *mut u8;
    fn r8a_free(p: *mut u8, size: usize, align: usize);
    fn r8a_pages(n: usize) -> *mut u8;
    fn r8a_free_pages(p: *mut u8, n: usize);
    fn r8a_trim();
}

/// 分配器（零大小）：全局分配器用 `#[global_allocator] static A: R8Alloc = R8Alloc;`。
#[derive(Clone, Copy, Debug, Default)]
pub struct R8Alloc;

impl R8Alloc {
    /// 一页的字节数（页按它对齐）
    pub const PAGE: usize = 256 << 10;

    /// `n` 页连续内存（按页对齐）；系统给不出时返回 None。
    pub fn pages(n: usize) -> Option<NonNull<u8>> {
        // SAFETY: 只是向分配器要内存
        NonNull::new(unsafe { r8a_pages(n) })
    }

    /// 还回 `n` 页。
    ///
    /// # Safety
    /// `p` 是 [`R8Alloc::pages`]`(n)` 给的、还没还过。
    pub unsafe fn free_pages(p: NonNull<u8>, n: usize) {
        // SAFETY: 调用者保证
        unsafe { r8a_free_pages(p.as_ptr(), n) }
    }

    /// 把所有空闲页的内存还给系统（地址留着复用；这期间还回来的页留到下一次）。
    pub fn trim() {
        // SAFETY: 只动空闲表里的页
        unsafe { r8a_trim() }
    }
}

// SAFETY: r8a_alloc 给出的块至少 layout.size() 字节、按 layout.align() 对齐，在 r8a_free 之前不会再给别人；
// r8a_free 收到的 size / align 与分配时相同，按它们找回同一个尺寸档
unsafe impl GlobalAlloc for R8Alloc {
    unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
        // SAFETY: 见上
        unsafe { r8a_alloc(layout.size(), layout.align()) }
    }

    unsafe fn dealloc(&self, p: *mut u8, layout: Layout) {
        // SAFETY: 见上
        unsafe { r8a_free(p, layout.size(), layout.align()) }
    }
}
