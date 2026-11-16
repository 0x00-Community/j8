//! r8alloc 的确定事实：块不重叠、内容不被别人改；任何线程都能释放别的线程分配的块；对齐满足；
//! 还回的页被复用、内存已归还系统（再拿到时全是 0）。测试二进制自己也用它当全局分配器。
use std::alloc::{GlobalAlloc, Layout};
use std::sync::mpsc;
use std::thread;

use r8alloc::R8Alloc;

#[global_allocator]
static GLOBAL: R8Alloc = R8Alloc;

/// 线程数
const THREADS: usize = 8;
/// 每个线程分配的块数
const EACH: usize = 20_000;
/// 线性同余的乘数与增量
const LCG: (u64, u64) = (6_364_136_223_846_793_005, 1_442_695_040_888_963_407);
/// 试的尺寸上限（跨过所有尺寸档进到按页分配）
const MAX_SIZE: usize = 300 << 10;

/// 一个块：地址、布局、写进去的特征字节
struct Block {
    at: usize,
    layout: Layout,
    tag: u8,
}

impl Block {
    fn check(&self) {
        // SAFETY: 块还没释放，大小是 layout.size()
        let bytes = unsafe { std::slice::from_raw_parts(self.at as *const u8, self.layout.size()) };
        assert!(bytes.iter().all(|&b| b == self.tag), "块 {:#x}（{:?}）被改过", self.at, self.layout);
    }
}

/// 8 个线程分配、写满特征值，交给下一个线程校验后释放：没有重叠、跨线程释放正常
#[test]
fn cross_thread_alloc_and_free() {
    let (senders, receivers): (Vec<_>, Vec<_>) = (0..THREADS).map(|_| mpsc::channel::<Vec<Block>>()).unzip();
    thread::scope(|s| {
        for (t, rx) in receivers.into_iter().enumerate() {
            let tx = senders[(t + 1) % THREADS].clone();
            s.spawn(move || {
                let mut rng = (t as u64 + 1).wrapping_mul(LCG.0);
                let mut mine = Vec::with_capacity(EACH);
                for i in 0..EACH {
                    rng = rng.wrapping_mul(LCG.0).wrapping_add(LCG.1);
                    // 大多是小块，偶尔大块
                    let size = if i % 97 == 0 { usize::try_from(rng >> 40).unwrap() % MAX_SIZE + 1 } else { usize::try_from(rng >> 52).unwrap() % 512 + 1 };
                    let align = 1 << (usize::try_from(rng >> 60).unwrap() % 8);
                    let layout = Layout::from_size_align(size, align).unwrap();
                    // SAFETY: 尺寸非零
                    let p = unsafe { GLOBAL.alloc(layout) };
                    assert!(!p.is_null() && (p as usize).is_multiple_of(align), "{layout:?} 没对齐");
                    let tag = u8::try_from((t * 31 + i) % 251).unwrap() + 1;
                    // SAFETY: 刚分到的 size 字节
                    unsafe { p.write_bytes(tag, size) };
                    mine.push(Block { at: p as usize, layout, tag });
                }
                for b in &mine {
                    b.check();
                }
                tx.send(mine).unwrap();
                drop(tx);
                for theirs in rx {
                    for b in theirs {
                        b.check();
                        // SAFETY: 别的线程分配、还没释放的块，布局与分配时相同
                        unsafe { GLOBAL.dealloc(b.at as *mut u8, b.layout) };
                    }
                }
            });
        }
        drop(senders);
    });
}

/// 还回的页立刻被同样页数的请求复用；trim 之后内存已经归还系统（再拿到时全是 0）
#[test]
fn pages_are_reused_and_returned() {
    let n = 3;
    let p = R8Alloc::pages(n).unwrap();
    assert_eq!(p.as_ptr() as usize % R8Alloc::PAGE, 0);
    // SAFETY: 刚分到的 n 页
    unsafe { p.as_ptr().write_bytes(0xAB, n * R8Alloc::PAGE) };
    // SAFETY: 还回刚才那 n 页
    unsafe { R8Alloc::free_pages(p, n) };
    R8Alloc::trim();
    let q = R8Alloc::pages(n).unwrap();
    assert_eq!(q, p, "同样页数的请求复用刚还回的页");
    // SAFETY: 刚分到的 n 页
    let bytes = unsafe { std::slice::from_raw_parts(q.as_ptr(), n * R8Alloc::PAGE) };
    assert!(bytes.iter().all(|&b| b == 0), "trim 之后内存已归还系统");
    // SAFETY: 还回
    unsafe { R8Alloc::free_pages(q, n) };
}

/// 大对齐（超过一页）也满足
#[test]
fn large_alignment() {
    for align in [1 << 12, 1 << 18, 1 << 20, 1 << 22] {
        let layout = Layout::from_size_align(align / 2 + 1, align).unwrap();
        // SAFETY: 尺寸非零
        let p = unsafe { GLOBAL.alloc(layout) };
        assert!(!p.is_null() && (p as usize).is_multiple_of(align), "对齐 {align} 没满足");
        // SAFETY: 刚分到的块
        unsafe {
            p.write_bytes(1, layout.size());
            GLOBAL.dealloc(p, layout);
        }
    }
}
