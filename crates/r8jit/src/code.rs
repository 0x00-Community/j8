use r8compiler::Native;
use r8executor::Exit;
use r8frame::Value;

use crate::helpers::Helpers;

/// 机器码入口的签名：本帧寄存器、累加器 ⇒ 出口码（见 [`Exit`]）
type Entry = unsafe extern "C" fn(*mut Value<'static>, *mut Value<'static>) -> u64;

/// 装进可执行内存的一个函数的机器码（只读可执行，drop 时放掉）。
pub(crate) struct Code {
    at: *mut libc::c_void,
    len: usize,
}

impl Code {
    /// 填好运行时函数的地址，映射成可执行内存。
    pub(crate) fn load(native: &Native) -> Option<Code> {
        let mut bytes = native.bytes.clone();
        for &(offset, helper) in &native.relocs {
            let addr = Helpers::address(helper)? as u64;
            bytes.get_mut(offset..offset + 8)?.copy_from_slice(&addr.to_le_bytes());
        }
        let len = bytes.len().max(1);
        // SAFETY: 新建的私有匿名映射，可写时拷进去，再改成只读可执行
        unsafe {
            let at = libc::mmap(core::ptr::null_mut(), len, libc::PROT_READ | libc::PROT_WRITE, libc::MAP_PRIVATE | libc::MAP_ANONYMOUS, -1, 0);
            if at == libc::MAP_FAILED {
                return None;
            }
            core::ptr::copy_nonoverlapping(bytes.as_ptr(), at.cast::<u8>(), bytes.len());
            if libc::mprotect(at, len, libc::PROT_READ | libc::PROT_EXEC) != 0 {
                libc::munmap(at, len);
                return None;
            }
            Some(Code { at, len })
        }
    }

    /// 在本帧的寄存器与累加器上跑。
    pub(crate) fn run(&self, regs: &mut [Value<'_>], acc: &mut Value<'_>) -> Exit {
        // SAFETY: 机器码是 r8compiler 为这个函数编的，只按 `ValueTag` 的布局读写 `regs`（不超过函数的寄存器数，
        // 执行器保证切片够长）与 `acc`，只写数 / 布尔 / 只有标签的值（不造 GC 指针），不分配、不碰别的内存
        let code = unsafe {
            let entry: Entry = core::mem::transmute::<*mut libc::c_void, Entry>(self.at);
            entry(regs.as_mut_ptr().cast(), core::ptr::from_mut(acc).cast())
        };
        Exit::from(code)
    }
}

impl Drop for Code {
    fn drop(&mut self) {
        // SAFETY: `load` 映射的那一段
        unsafe {
            libc::munmap(self.at, self.len);
        }
    }
}
