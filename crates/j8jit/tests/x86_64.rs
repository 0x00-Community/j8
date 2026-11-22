#![cfg(target_arch = "x86_64")]

use mlirpass::mlirformat::MlirOpt;
use j8jit::{sysv, X86_64Code, X86_64Emit};
use mlirpass::{Body, Ctx, Emit, Interfaces};

struct Mapped {
    at: *mut u8,
    len: usize,
}

impl Mapped {
    fn new(code: &X86_64Code, symbols: &[(&str, usize)]) -> Mapped {
        let mut bytes = code.bytes.clone();
        for r in &code.relocs {
            let (_, addr) = symbols.iter().find(|(n, _)| *n == r.symbol).unwrap_or_else(|| panic!("no symbol {}", r.symbol));
            bytes[r.offset..r.offset + 8].copy_from_slice(&(*addr as u64).to_le_bytes());
        }
        let len = bytes.len().max(1);
        let at = mem::alloc(len);
        assert!(!at.is_null(), "executable mapping");
        unsafe {
            core::ptr::copy_nonoverlapping(bytes.as_ptr(), at, bytes.len());
            assert!(mem::exec(at, len), "executable protection");
        }
        Mapped { at, len }
    }
}

impl Drop for Mapped {
    fn drop(&mut self) {
        mem::free(self.at, self.len);
    }
}

mod mem {
    #[cfg(windows)]
    const MEM_COMMIT: u32 = 0x1000;
    #[cfg(windows)]
    const MEM_RESERVE: u32 = 0x2000;
    #[cfg(windows)]
    const MEM_RELEASE: u32 = 0x8000;
    #[cfg(windows)]
    const PAGE_READWRITE: u32 = 0x04;
    #[cfg(windows)]
    const PAGE_EXECUTE_READ: u32 = 0x20;

    #[cfg(windows)]
    #[link(name = "kernel32")]
    unsafe extern "system" {
        fn VirtualAlloc(addr: *mut core::ffi::c_void, size: usize, flags: u32, prot: u32) -> *mut core::ffi::c_void;
        fn VirtualProtect(addr: *mut core::ffi::c_void, size: usize, prot: u32, old: *mut u32) -> i32;
        fn VirtualFree(addr: *mut core::ffi::c_void, size: usize, flags: u32) -> i32;
    }

    #[cfg(not(windows))]
    pub fn alloc(len: usize) -> *mut u8 {
        let at = unsafe { libc::mmap(core::ptr::null_mut(), len, libc::PROT_READ | libc::PROT_WRITE, libc::MAP_PRIVATE | libc::MAP_ANONYMOUS, -1, 0) };
        if at == libc::MAP_FAILED { core::ptr::null_mut() } else { at.cast() }
    }

    #[cfg(windows)]
    pub fn alloc(len: usize) -> *mut u8 {
        unsafe { VirtualAlloc(core::ptr::null_mut(), len, MEM_COMMIT | MEM_RESERVE, PAGE_READWRITE).cast() }
    }

    #[cfg(not(windows))]
    pub fn exec(at: *mut u8, len: usize) -> bool {
        unsafe { libc::mprotect(at.cast(), len, libc::PROT_READ | libc::PROT_EXEC) == 0 }
    }

    #[cfg(windows)]
    pub fn exec(at: *mut u8, len: usize) -> bool {
        let mut old = 0u32;
        unsafe { VirtualProtect(at.cast(), len, PAGE_EXECUTE_READ, &raw mut old) != 0 }
    }

    #[cfg(not(windows))]
    pub fn free(at: *mut u8, len: usize) {
        unsafe { libc::munmap(at.cast(), len) };
    }

    #[cfg(windows)]
    pub fn free(at: *mut u8, _len: usize) {
        unsafe { VirtualFree(at.cast(), 0, MEM_RELEASE) };
    }
}

sysv! {
    fn twice(x: i64) -> i64 {
        2 * x
    }
}

sysv! {
    fn hypot(x: f64, y: f64) -> f64 {
        (x * x + y * y).sqrt()
    }
}

fn compiled() -> Vec<X86_64Code> {
    let opt = MlirOpt::upstream();
    let interfaces = Interfaces::upstream();
    let body = Body::parse(Ctx::new(&opt, &interfaces), include_str!("x86_64/run.mlir")).unwrap();
    X86_64Emit.emit(&body, body.root).unwrap()
}

#[test]
fn runs() {
    let codes = compiled();
    let symbols = [("twice", twice as *const () as usize), ("hypot", hypot as *const () as usize)];
    let map: Vec<(String, Mapped)> = codes.iter().map(|c| (c.name.clone(), Mapped::new(c, &symbols))).collect();
    let f = |name: &str| map.iter().find(|(n, _)| n == name).unwrap_or_else(|| panic!("no function {name}")).1.at;
    unsafe {
        let poly: sysv!(fn(i64, i64) -> i64) = core::mem::transmute(f("poly"));
        let sum: sysv!(fn(i64) -> i64) = core::mem::transmute(f("sum"));
        let divs: sysv!(fn(i64, i64) -> i64) = core::mem::transmute(f("divs"));
        let narrow: sysv!(fn(i32, i32) -> i32) = core::mem::transmute(f("narrow"));
        let fmax: sysv!(fn(f64, f64) -> f64) = core::mem::transmute(f("fmax"));
        let unordered: sysv!(fn(f64, f64) -> i64) = core::mem::transmute(f("unordered"));
        let calls: sysv!(fn(i64, f64) -> f64) = core::mem::transmute(f("calls"));
        let memory: sysv!(fn(*mut i64, i64) -> i64) = core::mem::transmute(f("memory"));
        let bytes: sysv!(fn(*mut u8) -> i64) = core::mem::transmute(f("bytes"));
        let pressure: sysv!(fn(i64) -> i64) = core::mem::transmute(f("pressure"));
        let across: sysv!(fn(i64, f64) -> f64) = core::mem::transmute(f("across"));
        let fsum: sysv!(fn(i64, f64) -> f64) = core::mem::transmute(f("fsum"));
        for (a, b) in [(3, 4), (-7, 5), (1 << 40, -3), (0, 0)] {
            assert_eq!(poly(a, b), a.wrapping_mul(a).wrapping_add(b.wrapping_mul(3)) ^ (a - b), "poly({a}, {b})");
        }
        for n in [0, 1, 10, 1000] {
            assert_eq!(sum(n), (0..n).sum::<i64>(), "sum({n})");
        }
        for (a, b) in [(7, 2), (-7, 2), (7, -2), (-7, -2)] {
            assert_eq!(divs(a, b), (a / b) * 1000 + (a % b), "divs({a}, {b})");
        }
        for (a, b) in [(5, 3), (-5, 3), (i32::MAX, 1), (-1, -2)] {
            assert_eq!(narrow(a, b), i32::from(a < b) + (a.wrapping_add(b) >> 1), "narrow({a}, {b})");
        }
        for (a, b) in [(1.5, 2.5), (-0.5, -1.0), (3.0, 3.0)] {
            assert_eq!(fmax(a, b), if a > b { a } else { b } * 2.0, "fmax({a}, {b})");
        }
        assert_eq!(unordered(f64::NAN, 1.0), 1);
        assert_eq!(unordered(2.0, 1.0), 0);
        assert_eq!(calls(5, 4.0), (hypot(twice(5) as f64, 4.0) * 1000.0).trunc());
        let mut cells = [1i64, 2, 3, 4];
        assert_eq!(memory(cells.as_mut_ptr(), 4), 10);
        assert_eq!(cells, [2, 4, 6, 8]);
        let mut raw = [200u8, 100, 7];
        assert_eq!(bytes(raw.as_mut_ptr()), 300);
        assert_eq!(raw, [200, 100, 44]);
        for x in [1i64, -3, 1 << 33] {
            let v: Vec<i64> = (1..=20).map(|k| x.wrapping_mul(k)).collect();
            let mut acc = v[0];
            for i in 1..20 {
                acc = (acc ^ v[i]).wrapping_add(v[20 - i]);
            }
            assert_eq!(pressure(x), acc, "pressure({x})");
        }
        for (n, y) in [(3i64, 1.5f64), (-2, 0.25)] {
            let want = ((n * 7 + twice(twice(n))) as f64 + y * y) + hypot(y, y);
            assert_eq!(across(n, y), want, "across({n}, {y})");
        }
        for (n, step) in [(0i64, 1.0f64), (10, 0.5), (1000, 0.001)] {
            let want = (0..n).fold(0.0f64, |a, i| a + i as f64 * step);
            assert_eq!(fsum(n, step), want, "fsum({n}, {step})");
        }
    }
}
