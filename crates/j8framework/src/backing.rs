//! ArrayBuffer 的字节放在哪：引擎自己的、别人拥有的（宿主映射、C 缓冲区）、几个 isolate 共享的。三种都当一个 `[u8]` 读写，
//! 内置对象不用管是哪种。
use alloc::alloc::{Layout, alloc_zeroed, dealloc};
use alloc::boxed::Box;
use alloc::sync::Arc;
use alloc::vec::Vec;
use core::fmt;
use core::ops::{Deref, DerefMut};
use core::ptr::NonNull;
use core::sync::atomic::{AtomicU8, AtomicU16, AtomicU32, AtomicU64, AtomicUsize, Ordering};

/// 共享内存的对齐：对齐下标上的整数元素（最多 8 字节）都自然对齐，Atomics 能用真的原子指令
const SHARED_ALIGN: usize = 16;

/// ArrayBuffer / SharedArrayBuffer 的字节。
pub enum Backing {
    /// 引擎分配的，或不拷贝直接交进来的 `Vec`
    Owned(Vec<u8>),
    /// 别人拥有的内存；buffer detach 或被回收时释放
    External(External),
    /// 几个 isolate（线程）同时看见的内存
    Shared(Arc<SharedMemory>),
}

impl Backing {
    /// 改长度（新字节为零）。自有的随便改，共享的只能在容量内增长，外部的长度不变：做不到时返回 `false`。
    pub fn resize(&mut self, len: usize) -> bool {
        match self {
            Self::Owned(v) => {
                v.resize(len, 0);
                true
            }
            Self::External(e) => e.len == len,
            Self::Shared(s) => s.grow(len),
        }
    }

    /// 变成 `Vec`：自有的直接移出，其余拷贝。
    pub fn into_vec(self) -> Vec<u8> {
        match self {
            Self::Owned(v) => v,
            other => other.to_vec(),
        }
    }

    /// 是共享内存时取出它
    pub fn shared(&self) -> Option<&Arc<SharedMemory>> {
        match self {
            Self::Shared(s) => Some(s),
            _ => None,
        }
    }

    /// 对 `at` 处 `size` 字节的小端整数读-改-写：`f` 拿旧值给新值（`None` 不改），返回旧值。共享内存上是一步原子操作
    /// （别的线程只会看到旧值或新值，不会半新半旧），其余就是普通读写。
    pub fn update(&mut self, at: usize, size: usize, f: impl Fn(u64) -> Option<u64>) -> u64 {
        if let Self::Shared(s) = self {
            return s.update(at, size, f);
        }
        let cell = &mut self[at..at + size];
        let mut x = [0u8; 8];
        x[..size].copy_from_slice(cell);
        let old = u64::from_le_bytes(x);
        if let Some(new) = f(old) {
            cell.copy_from_slice(&new.to_le_bytes()[..size]);
        }
        old
    }
}

impl Deref for Backing {
    type Target = [u8];

    fn deref(&self) -> &[u8] {
        match self {
            Self::Owned(v) => v,
            Self::External(e) => e,
            // SAFETY: Arc 活着时 `ptr` 起 `len` 字节一直有效；别的线程并发写属于 JS 内存模型的 unordered 访问
            Self::Shared(s) => unsafe { core::slice::from_raw_parts(s.ptr.as_ptr(), s.len()) },
        }
    }
}

impl DerefMut for Backing {
    fn deref_mut(&mut self) -> &mut [u8] {
        match self {
            Self::Owned(v) => v,
            Self::External(e) => e,
            // SAFETY: 同 `deref`
            Self::Shared(s) => unsafe { core::slice::from_raw_parts_mut(s.ptr.as_ptr(), s.len()) },
        }
    }
}

impl From<Vec<u8>> for Backing {
    fn from(v: Vec<u8>) -> Self {
        Self::Owned(v)
    }
}

impl From<External> for Backing {
    fn from(e: External) -> Self {
        Self::External(e)
    }
}

impl From<Arc<SharedMemory>> for Backing {
    fn from(s: Arc<SharedMemory>) -> Self {
        Self::Shared(s)
    }
}

impl fmt::Debug for Backing {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let kind = match self {
            Self::Owned(_) => "Owned",
            Self::External(_) => "External",
            Self::Shared(_) => "Shared",
        };
        write!(f, "{kind}({} bytes)", self.len())
    }
}

/// ArrayBuffer 只看不拥有的内存：`owner` 保活，随 buffer 一起 drop 时释放（在哪个线程 drop 就在哪释放）。
pub struct External {
    ptr: NonNull<u8>,
    len: usize,
    /// 不读，drop 它就释放内存
    #[expect(dead_code, reason = "只为随 buffer 一起 drop")]
    owner: Box<dyn Send>,
}

// SAFETY: 内存归 `owner`（`Send`），指针没有别人持有
unsafe impl Send for External {}
// SAFETY: `&External` 只能读到字节，碰不到 `owner`
unsafe impl Sync for External {}

impl External {
    /// owner 给出的切片（`Box<[u8]>`、映射守卫 …）原地使用。先把 owner 装箱，切片地址就不会动。
    pub fn new<T: AsMut<[u8]> + Send + 'static>(owner: T) -> Self {
        let mut owner = Box::new(owner);
        let bytes = (*owner).as_mut();
        let (ptr, len) = (NonNull::new(bytes.as_mut_ptr()).unwrap_or(NonNull::dangling()), bytes.len());
        Self { ptr, len, owner }
    }

    /// `ptr` 起 `len` 字节（同 `napi_create_external_arraybuffer` / V8 `new_backing_store_from_ptr`），`owner` drop 时释放。
    ///
    /// # Safety
    /// `owner` drop 之前 `ptr` 起 `len` 字节可读写，期间没有别人写。
    pub unsafe fn from_raw(ptr: *mut u8, len: usize, owner: Box<dyn Send>) -> Self {
        Self { ptr: NonNull::new(ptr).unwrap_or(NonNull::dangling()), len, owner }
    }
}

impl Deref for External {
    type Target = [u8];

    fn deref(&self) -> &[u8] {
        // SAFETY: `owner` 活着时 `len` 字节有效（`new` / `from_raw`）
        unsafe { core::slice::from_raw_parts(self.ptr.as_ptr(), self.len) }
    }
}

impl DerefMut for External {
    fn deref_mut(&mut self) -> &mut [u8] {
        // SAFETY: 同 `deref`，且只能经 `&mut self` 进来
        unsafe { core::slice::from_raw_parts_mut(self.ptr.as_ptr(), self.len) }
    }
}

/// isolate 之间共享的内存：每个 isolate 里各有一个 SharedArrayBuffer 看它。一开始就按最大长度分配（清零、永不搬动），
/// 增长只改长度，别的线程手里的视图一直有效。
pub struct SharedMemory {
    ptr: NonNull<u8>,
    len: AtomicUsize,
    cap: usize,
    /// 能长到多长；`None` 为定长
    max: Option<usize>,
}

// SAFETY: 分配归它所有、只经它读写；长度是原子的
unsafe impl Send for SharedMemory {}
// SAFETY: 同上；字节的并发访问归 JS 内存模型管（Atomics 走 `update`）
unsafe impl Sync for SharedMemory {}

impl SharedMemory {
    /// `len` 个零字节，能长到 `max`（`None` 为定长）；分配失败返回 `None`。
    pub fn new(len: usize, max: Option<usize>) -> Option<Arc<Self>> {
        let cap = max.unwrap_or(len).max(len);
        let ptr = if cap == 0 {
            NonNull::<u64>::dangling().cast()
        } else {
            let layout = Layout::from_size_align(cap, SHARED_ALIGN).ok()?;
            // SAFETY: `cap` 非零
            NonNull::new(unsafe { alloc_zeroed(layout) })?
        };
        Some(Arc::new(Self { ptr, len: AtomicUsize::new(len), cap, max }))
    }

    /// 当前长度
    pub fn len(&self) -> usize {
        self.len.load(Ordering::SeqCst)
    }

    /// 长度是否为零
    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }

    /// 能长到多长（`maxByteLength`）；`None` 为定长
    pub fn max(&self) -> Option<usize> {
        self.max
    }

    /// 长到 `len`（不缩；期间别的线程可能已经长得更多）：超过容量或小于当前长度返回 `false`。
    pub fn grow(&self, len: usize) -> bool {
        len <= self.cap && self.len.fetch_update(Ordering::SeqCst, Ordering::SeqCst, |cur| (len >= cur).then_some(len)).is_ok()
    }

    /// [`Backing::update`]：一个原子 CAS 循环。
    fn update(&self, at: usize, size: usize, f: impl Fn(u64) -> Option<u64>) -> u64 {
        assert!(at + size <= self.len() && at.is_multiple_of(size), "原子访问越界或未对齐");
        // SAFETY: 在范围内（已查）、自然对齐（`at` 是 `size` 的倍数，基址按 `SHARED_ALIGN` 对齐），且只按原子或 unordered 字节访问
        let p = unsafe { self.ptr.as_ptr().add(at) };
        macro_rules! rmw {
            ($atomic:ty, $int:ty) => {{
                // SAFETY: 见上
                #[allow(clippy::cast_ptr_alignment, reason = "自然对齐，见上")]
                let cell = unsafe { <$atomic>::from_ptr(p.cast::<$int>()) };
                let mut old = cell.load(Ordering::SeqCst);
                loop {
                    let Some(new) = f(u64::from(<$int>::from_le(old))) else { return u64::from(<$int>::from_le(old)) };
                    #[allow(clippy::cast_possible_truncation, reason = "元素只有 `size` 字节")]
                    let new = (new as $int).to_le();
                    match cell.compare_exchange_weak(old, new, Ordering::SeqCst, Ordering::SeqCst) {
                        Ok(_) => return u64::from(<$int>::from_le(old)),
                        Err(cur) => old = cur,
                    }
                }
            }};
        }
        match size {
            1 => rmw!(AtomicU8, u8),
            2 => rmw!(AtomicU16, u16),
            4 => rmw!(AtomicU32, u32),
            8 => rmw!(AtomicU64, u64),
            _ => unreachable!("整数元素只有 1 / 2 / 4 / 8 字节"),
        }
    }
}

impl Drop for SharedMemory {
    fn drop(&mut self) {
        if self.cap != 0 {
            // SAFETY: `new` 里按这个布局分配
            unsafe { dealloc(self.ptr.as_ptr(), Layout::from_size_align_unchecked(self.cap, SHARED_ALIGN)) }
        }
    }
}

impl fmt::Debug for SharedMemory {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "SharedMemory({} of {} bytes)", self.len(), self.cap)
    }
}
