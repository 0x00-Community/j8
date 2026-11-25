use alloc::sync::Arc;
use core::sync::atomic::{AtomicU32, Ordering};

use j8bytecode::{Decoded, WasmBytecode};

use super::entity::{Code, WasmInstance};
use alloc::vec::Vec;

use super::{Activation, WasmScalar, WasmStore, WasmTrap};

#[cfg(all(windows, target_arch = "x86_64"))]
macro_rules! native {
    ($(#[$m:meta])* $v:vis fn $name:ident($($p:tt)*) -> $ret:ty $body:block) => { $(#[$m])* $v extern "sysv64" fn $name($($p)*) -> $ret $body };
    (type $name:ident = fn($($p:ty),*) -> $ret:ty;) => { type $name = unsafe extern "sysv64" fn($($p),*) -> $ret; };
}

#[cfg(not(all(windows, target_arch = "x86_64")))]
macro_rules! native {
    ($(#[$m:meta])* $v:vis fn $name:ident($($p:tt)*) -> $ret:ty $body:block) => { $(#[$m])* $v extern "C" fn $name($($p)*) -> $ret $body };
    (type $name:ident = fn($($p:ty),*) -> $ret:ty;) => { type $name = unsafe extern "C" fn($($p),*) -> $ret; };
}

native! { type Entry = fn(*mut WasmCtx, *mut u64) -> u64; }

#[repr(C)]
pub struct WasmCtx {
    io: usize,
    mem_base: u64,
    mem_len: u64,
    status: u64,
    trap: u64,
    store: usize,
    frame: usize,
    io_at: usize,
    depth: u64,
    osr: u64,
}

impl WasmCtx {
    pub const IO: i32 = 0;
    pub const MEM_BASE: i32 = 8;
    pub const MEM_LEN: i32 = 16;
    pub const STATUS: i32 = 24;
    pub const TRAP: i32 = 32;
    pub const OSR: i32 = 72;
    pub const RETURNED: u64 = 0;
    pub const TRAPPED: u64 = 1;
    pub const THREW: u64 = 2;
    pub const OUT_OF_BOUNDS: u64 = WasmTrap::OutOfBoundsMemory as u64;
    pub const UNSUPPORTED: u64 = WasmTrap::Unsupported as u64;
    const NEST: u64 = 96;
    const NATIVE: usize = usize::MAX;

    fn new() -> Self {
        Self { io: 0, mem_base: 0, mem_len: 0, status: 0, trap: 0, store: 0, frame: 0, io_at: 0, depth: 0, osr: 0 }
    }
}

impl Default for WasmCtx {
    fn default() -> Self {
        Self::new()
    }
}

pub struct WasmJitFunction<'a> {
    pub ops: &'a [Decoded<WasmBytecode>],
    pub at: &'a [u32],
    pub bits: &'a [u64],
    pub tables: &'a [alloc::boxed::Box<[u32]>],
    pub params: u32,
    pub results: u32,
    pub registers: u32,
    pub memory64: Option<bool>,
    pub step: u64,
    pub scalar: u64,
    pub call: u64,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct WasmMachine {
    pub call: u64,
    pub osr: u64,
}

pub trait WasmTier {
    fn threshold(&self) -> u32;

    fn compile(&self, f: &WasmJitFunction<'_>) -> Option<WasmMachine>;
}

struct Frame<'scope> {
    instance: &'scope WasmInstance<'scope>,
    code: Arc<Code>,
}

const TRAPS: [WasmTrap; 18] = [
    WasmTrap::Unreachable,
    WasmTrap::IntegerDivideByZero,
    WasmTrap::IntegerOverflow,
    WasmTrap::InvalidConversion,
    WasmTrap::OutOfBoundsMemory,
    WasmTrap::OutOfBoundsTable,
    WasmTrap::UndefinedElement,
    WasmTrap::UninitializedElement,
    WasmTrap::IndirectCallTypeMismatch,
    WasmTrap::NullReference,
    WasmTrap::CallStackExhausted,
    WasmTrap::NullStructure,
    WasmTrap::NullArray,
    WasmTrap::NullI31,
    WasmTrap::CastFailure,
    WasmTrap::OutOfBoundsArray,
    WasmTrap::Exception,
    WasmTrap::Unsupported,
];

impl From<u64> for WasmTrap {
    fn from(code: u64) -> Self {
        TRAPS.into_iter().find(|&t| t as u64 == code).unwrap_or(WasmTrap::Unsupported)
    }
}

pub struct WasmJit;

impl WasmJit {
    native! {
        fn step(ctx: *mut WasmCtx, pc: u64, regs: *mut u64) -> u64 {
            unsafe { WasmJit::stepped(&mut *ctx, pc, regs) }
        }
    }

    native! {
        fn scalar(ctx: *mut WasmCtx, op: u64, a: u64, b: u64) -> u64 {
            let code = u16::try_from(op).unwrap_or(u16::MAX);
            let [page, index] = code.to_be_bytes();
            let r = WasmBytecode::from_page(page, index).map_or(Err(WasmTrap::Unsupported), |op| WasmScalar::eval(op, a, b).and_then(|v| v.ok_or(WasmTrap::Unsupported)));
            match r {
                Ok(v) => v,
                Err(t) => {
                    let ctx = unsafe { &mut *ctx };
                    ctx.trap = t as u64;
                    ctx.status = WasmCtx::TRAPPED;
                    0
                }
            }
        }
    }

    native! {
        fn call(ctx: *mut WasmCtx, index: u64, args: *mut u64) -> u64 {
            unsafe { WasmJit::called(&mut *ctx, index, args) }
        }
    }

    unsafe fn called(ctx: &mut WasmCtx, index: u64, args: *mut u64) -> u64 {
        let store = unsafe { &mut *(ctx.store as *mut WasmStore<'static, 'static>) };
        let frame = unsafe { &*(ctx.frame as *const Frame<'static>) };
        let callee = frame.instance.func(usize::try_from(index).unwrap_or(usize::MAX));
        if let Some(inst) = callee.instance.get() {
            let instance = inst.get();
            let code = instance.module.code(callee.index);
            if let Some(WasmMachine { call: e, .. }) = WasmJit::jitted(store, &code) {
                let saved = (ctx.io, ctx.io_at, ctx.frame, ctx.mem_base, ctx.mem_len);
                let inner = Frame { instance, code };
                ctx.io = args as usize;
                ctx.io_at = WasmCtx::NATIVE;
                ctx.frame = core::ptr::from_ref(&inner) as usize;
                WasmJit::refresh(ctx, &mut store.stack, instance);
                ctx.depth += 1;
                let run: Entry = unsafe { core::mem::transmute::<usize, Entry>(usize::try_from(e).unwrap_or(0)) };
                let status = unsafe { run(ctx, args) };
                ctx.depth -= 1;
                (ctx.io, ctx.io_at, ctx.frame, ctx.mem_base, ctx.mem_len) = saved;
                WasmJit::refresh(ctx, &mut store.stack, frame.instance);
                if status != WasmCtx::RETURNED {
                    ctx.status = status;
                }
                return status;
            }
        }
        let ty = store.func_type(callee);
        let (params, results) = (ty.params.len(), ty.results.len());
        let fp = store.stack.len();
        let input = unsafe { core::slice::from_raw_parts(args, params) };
        store.stack.extend_from_slice(input);
        store.vstack.resize(fp + params, 0);
        store.reserve(fp + results);
        let r = store.enter(callee, fp);
        if r.is_ok() {
            let out = unsafe { core::slice::from_raw_parts_mut(args, results) };
            out.copy_from_slice(&store.stack[fp..fp + results]);
        }
        store.truncate(fp);
        WasmJit::refresh(ctx, &mut store.stack, frame.instance);
        WasmJit::status(ctx, r)
    }

    fn status(ctx: &mut WasmCtx, r: Result<(), WasmTrap>) -> u64 {
        match r {
            Ok(()) => WasmCtx::RETURNED,
            Err(WasmTrap::Exception) => {
                ctx.status = WasmCtx::THREW;
                WasmCtx::THREW
            }
            Err(t) => {
                ctx.trap = t as u64;
                ctx.status = WasmCtx::TRAPPED;
                WasmCtx::TRAPPED
            }
        }
    }

    unsafe fn stepped(ctx: &mut WasmCtx, pc: u64, regs: *mut u64) -> u64 {
        let store = unsafe { &mut *(ctx.store as *mut WasmStore<'static, 'static>) };
        let frame = unsafe { &*(ctx.frame as *const Frame<'static>) };
        let n = frame.code.registers as usize;
        let fp = store.stack.len();
        let regs = unsafe { core::slice::from_raw_parts_mut(regs, n) };
        store.stack.extend_from_slice(regs);
        store.vstack.resize(fp + n, 0);
        let act = Activation { instance: frame.instance, code: frame.code.clone(), fp, pc: usize::try_from(pc).unwrap_or(usize::MAX) };
        let r = store.step(act);
        regs.copy_from_slice(&store.stack[fp..fp + n]);
        store.truncate(fp);
        WasmJit::refresh(ctx, &mut store.stack, frame.instance);
        WasmJit::status(ctx, r)
    }

    fn refresh(ctx: &mut WasmCtx, stack: &mut [u64], instance: &WasmInstance<'_>) {
        if ctx.io_at != WasmCtx::NATIVE {
            ctx.io = stack.as_mut_ptr().wrapping_add(ctx.io_at) as usize;
        }
        let (base, len) = instance.memories.first().and_then(j8framework::GcSlot::get).map_or((0, 0), |m| {
            let d = m.get().data();
            (d.as_mut_ptr() as usize as u64, d.len() as u64)
        });
        ctx.mem_base = base;
        ctx.mem_len = len;
    }
}

impl<'a> WasmStore<'a, '_> {
    #[must_use]
    pub fn with_tier(mut self, tier: &'a dyn WasmTier) -> Self {
        self.tier = Some(tier);
        self
    }
}

impl WasmJit {
    pub fn jitted(store: &WasmStore<'_, '_>, code: &Code) -> Option<WasmMachine> {
        WasmJit::hot(store, code, &code.calls)
    }

    pub fn looped(store: &WasmStore<'_, '_>, code: &Code) -> Option<WasmMachine> {
        WasmJit::hot(store, code, &code.loops)
    }

    fn hot(store: &WasmStore<'_, '_>, code: &Code, counter: &AtomicU32) -> Option<WasmMachine> {
        let tier = store.tier?;
        if code.vector || !code.handlers.is_empty() || store.ctx.depth >= WasmCtx::NEST {
            return None;
        }
        if let Some(&m) = code.jit.get() {
            return (m.call != 0).then_some(m);
        }
        if counter.fetch_add(1, Ordering::Relaxed) < tier.threshold() {
            return None;
        }
        let f = WasmJitFunction {
            ops: &code.ops,
            at: &code.at,
            bits: &code.bits,
            tables: &code.tables,
            params: code.params,
            results: code.results,
            registers: code.registers,
            memory64: code.memory64,
            step: WasmJit::step as *const () as usize as u64,
            scalar: WasmJit::scalar as *const () as usize as u64,
            call: WasmJit::call as *const () as usize as u64,
        };
        let m = *code.jit.get_or_init(|| tier.compile(&f).unwrap_or_default());
        (m.call != 0).then_some(m)
    }

    pub fn machine<'scope>(store: &mut WasmStore<'_, 'scope>, entry: u64, instance: &'scope WasmInstance<'scope>, code: Arc<Code>, fp: usize) -> Result<(), WasmTrap> {
        store.reserve(fp + code.params.max(code.results) as usize);
        WasmJit::run(store, entry, instance, code, fp)
    }

    pub fn osr<'scope>(store: &mut WasmStore<'_, 'scope>, frames: &mut Vec<Activation<'scope>>) -> Result<(), WasmTrap> {
        let Some(f) = frames.last() else { return Ok(()) };
        let Some(m) = WasmJit::looped(store, &f.code) else { return Ok(()) };
        let (instance, code, fp) = (f.instance, f.code.clone(), f.fp);
        store.ctx.osr = u64::try_from(f.pc).unwrap_or(u64::MAX);
        WasmJit::run(store, m.osr, instance, code, fp)?;
        frames.pop();
        Ok(())
    }

    fn run<'scope>(store: &mut WasmStore<'_, 'scope>, entry: u64, instance: &'scope WasmInstance<'scope>, code: Arc<Code>, fp: usize) -> Result<(), WasmTrap> {
        let this = store;
        let frame = Frame { instance, code };
        let saved = (this.ctx.io, this.ctx.io_at, this.ctx.frame, this.ctx.mem_base, this.ctx.mem_len);
        this.ctx.io_at = fp;
        this.ctx.frame = core::ptr::from_ref(&frame) as usize;
        this.ctx.store = core::ptr::from_mut(this) as usize;
        this.ctx.status = WasmCtx::RETURNED;
        WasmJit::refresh(&mut this.ctx, &mut this.stack, instance);
        this.ctx.depth += 1;
        let run: Entry = unsafe { core::mem::transmute::<usize, Entry>(usize::try_from(entry).unwrap_or(0)) };
        let ctx = core::ptr::from_mut(&mut *this.ctx);
        let io = this.ctx.io as *mut u64;
        let status = unsafe { run(ctx, io) };
        this.ctx.depth -= 1;
        (this.ctx.io, this.ctx.io_at, this.ctx.frame, this.ctx.mem_base, this.ctx.mem_len) = saved;
        if this.ctx.frame != 0 {
            let outer = unsafe { &*(this.ctx.frame as *const Frame<'_>) };
            WasmJit::refresh(&mut this.ctx, &mut this.stack, outer.instance);
        }
        match status {
            WasmCtx::RETURNED => Ok(()),
            WasmCtx::THREW => Err(WasmTrap::Exception),
            _ => Err(WasmTrap::from(this.ctx.trap)),
        }
    }
}
