use r8compiler::Native;
use r8executor::Exit;
use r8framework::Value;

use crate::helpers::Helpers;

/// Signature of the machine-code entry: this frame's registers, accumulator, where to enter (0 the start / `h + 1` a
/// loop header) ⇒ exit code (see [`Exit`])
type Entry = unsafe extern "C" fn(*mut Value<'static>, *mut Value<'static>, u64) -> u64;

/// One function's machine code installed in executable memory (read-only executable, released on drop).
pub(crate) struct Code {
    at: *mut libc::c_void,
    len: usize,
}

impl Code {
    /// Fills in the runtime function addresses and maps it as executable memory.
    pub(crate) fn load(native: &Native) -> Option<Code> {
        let mut bytes = native.bytes.clone();
        for &(offset, helper) in &native.relocs {
            let addr = Helpers::address(helper)? as u64;
            bytes.get_mut(offset..offset + 8)?.copy_from_slice(&addr.to_le_bytes());
        }
        let len = bytes.len().max(1);
        // SAFETY: a freshly created private anonymous mapping; copy in while writable, then make it read-only
        // executable
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

    /// Runs on this frame's registers and accumulator, entering at `start` (0 the start / `h + 1` loop header `h`).
    pub(crate) fn run(&self, regs: &mut [Value<'_>], acc: &mut Value<'_>, start: u64) -> Exit {
        // SAFETY: the machine code was compiled by r8compiler for this function; it only reads and writes `regs` (no
        // more than the function's register count; the executor guarantees the slice is long enough) and `acc` per the
        // `ValueBits` layout, only writes numbers / booleans / constant words (creates no GC pointers), does not
        // allocate, and touches no other memory
        let code = unsafe {
            let entry: Entry = core::mem::transmute::<*mut libc::c_void, Entry>(self.at);
            entry(regs.as_mut_ptr().cast(), core::ptr::from_mut(acc).cast(), start)
        };
        Exit::from(code)
    }
}

impl Drop for Code {
    fn drop(&mut self) {
        // SAFETY: the range mapped by `load`
        unsafe {
            libc::munmap(self.at, self.len);
        }
    }
}
