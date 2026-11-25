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
    mem_base: u64,
    mem_len: u64,
    status: u64,
    trap: u64,
    depth: u64,
    osr: u64,
    store: usize,
    instance: usize,
}

impl WasmCtx {
    pub const MEM_BASE: i32 = 0;
    pub const MEM_LEN: i32 = 8;
    pub const STATUS: i32 = 16;
    pub const TRAP: i32 = 24;
    pub const DEPTH: i32 = 32;
    pub const OSR: i32 = 40;
    pub const RETURNED: u64 = 0;
    pub const TRAPPED: u64 = 1;
    pub const THREW: u64 = 2;
    pub const OUT_OF_BOUNDS: u64 = WasmTrap::OutOfBoundsMemory as u64;
    pub const UNSUPPORTED: u64 = WasmTrap::Unsupported as u64;
    pub const NEST: u64 = 96;
    const INLINE_IO: usize = 16;

    fn new() -> Self {
        Self { mem_base: 0, mem_len: 0, status: 0, trap: 0, depth: 0, osr: 0, store: 0, instance: 0 }
    }

    fn store(&self) -> &'static mut WasmStore<'static, 'static> {
        unsafe { &mut *(self.store as *mut WasmStore<'static, 'static>) }
    }

    fn instance(&self) -> &'static WasmInstance<'static> {
        unsafe { &*(self.instance as *const WasmInstance<'static>) }
    }

    fn enter(&mut self, instance: &WasmInstance<'_>) {
        self.instance = core::ptr::from_ref(instance) as usize;
        self.refresh();
    }

    fn refresh(&mut self) {
        if self.instance == 0 {
            return;
        }
        let (base, len) = self.instance().memories.first().and_then(j8framework::GcSlot::get).map_or((0, 0), |m| {
            let d = m.get().data();
            (d.as_mut_ptr() as usize as u64, d.len() as u64)
        });
        self.mem_base = base;
        self.mem_len = len;
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
    pub imports: u32,
    pub entries: u64,
    pub code: u64,
    pub step: u64,
    pub scalar: u64,
    pub call: u64,
    pub global: u64,
    pub indirect: u64,
    pub call_ref: u64,
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
        fn step(ctx: *mut WasmCtx, pc: u64, regs: *mut u64, code: *const Code) -> u64 {
            unsafe { WasmJit::stepped(&mut *ctx, pc, regs, code) }
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
            let ctx = unsafe { &mut *ctx };
            let callee = ctx.instance().func(usize::try_from(index).unwrap_or(usize::MAX));
            unsafe { WasmJit::invoke(ctx, callee, args) }
        }
    }

    native! {
        fn global(ctx: *mut WasmCtx, index: u64, value: u64, set: u64) -> u64 {
            let ctx = unsafe { &mut *ctx };
            let g = ctx.instance().global(usize::try_from(index).unwrap_or(usize::MAX));
            if set == 0 {
                g.get().0
            } else {
                g.set(ctx.store().mc, (value, 0));
                0
            }
        }
    }

    native! {
        fn indirect(ctx: *mut WasmCtx, packed: u64, at: u64, args: *mut u64) -> u64 {
            let ctx = unsafe { &mut *ctx };
            let instance = ctx.instance();
            let (table, ty) = (packed >> u32::BITS, packed & u64::from(u32::MAX));
            let t = instance.table(usize::try_from(table).unwrap_or(usize::MAX));
            let i = t.index(at);
            let callee = t
                .cells()
                .get(usize::try_from(i).unwrap_or(usize::MAX))
                .ok_or(WasmTrap::UndefinedElement)
                .and_then(|e| ctx.store().callee(instance, j8bytecode::OperandValue::try_from(ty).unwrap_or(0), e.get()));
            match callee {
                Ok(c) => unsafe { WasmJit::invoke(ctx, c, args) },
                Err(e) => WasmJit::status(ctx, Err(e)),
            }
        }
    }

    native! {
        fn call_ref(ctx: *mut WasmCtx, _unused: u64, r: u64, args: *mut u64) -> u64 {
            let ctx = unsafe { &mut *ctx };
            match super::entity::Ptr::get::<super::WasmFunc<'static>>(r) {
                Some(c) => unsafe { WasmJit::invoke(ctx, c, args) },
                None => WasmJit::status(ctx, Err(WasmTrap::NullReference)),
            }
        }
    }

    unsafe fn invoke(ctx: &mut WasmCtx, callee: &'static super::WasmFunc<'static>, args: *mut u64) -> u64 {
        let store = ctx.store();
        let caller = ctx.instance;
        if let Some(inst) = callee.instance.get() {
            let instance = inst.get();
            let code = instance.module.code(callee.index);
            if let Some(WasmMachine { call: e, .. }) = WasmJit::jitted(store, &code) {
                ctx.enter(instance);
                ctx.depth += 1;
                let run: Entry = unsafe { core::mem::transmute::<usize, Entry>(usize::try_from(e).unwrap_or(0)) };
                let status = unsafe { run(ctx, args) };
                ctx.depth -= 1;
                ctx.instance = caller;
                ctx.refresh();
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
        ctx.refresh();
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

    unsafe fn stepped(ctx: &mut WasmCtx, pc: u64, regs: *mut u64, code: *const Code) -> u64 {
        let store = ctx.store();
        let code = unsafe {
            Arc::increment_strong_count(code);
            Arc::from_raw(code)
        };
        let n = code.registers as usize;
        let fp = store.stack.len();
        let regs = unsafe { core::slice::from_raw_parts_mut(regs, n) };
        store.stack.extend_from_slice(regs);
        store.vstack.resize(fp + n, 0);
        let act = Activation { instance: ctx.instance(), code, fp, pc: usize::try_from(pc).unwrap_or(usize::MAX) };
        let r = store.step(act);
        regs.copy_from_slice(&store.stack[fp..fp + n]);
        store.truncate(fp);
        ctx.refresh();
        WasmJit::status(ctx, r)
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
    pub fn jitted(store: &WasmStore<'_, '_>, code: &Arc<Code>) -> Option<WasmMachine> {
        WasmJit::hot(store, code, &code.calls)
    }

    pub fn looped(store: &WasmStore<'_, '_>, code: &Arc<Code>) -> Option<WasmMachine> {
        WasmJit::hot(store, code, &code.loops)
    }

    fn hot(store: &WasmStore<'_, '_>, code: &Arc<Code>, counter: &AtomicU32) -> Option<WasmMachine> {
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
            imports: code.imports,
            entries: code.entries as u64,
            code: Arc::as_ptr(code) as usize as u64,
            step: WasmJit::step as *const () as usize as u64,
            scalar: WasmJit::scalar as *const () as usize as u64,
            call: WasmJit::call as *const () as usize as u64,
            global: WasmJit::global as *const () as usize as u64,
            indirect: WasmJit::indirect as *const () as usize as u64,
            call_ref: WasmJit::call_ref as *const () as usize as u64,
        };
        let m = *code.jit.get_or_init(|| {
            let m = tier.compile(&f).unwrap_or_default();
            if let Some(cell) = code.entry() {
                cell.store(m.call, Ordering::Release);
            }
            m
        });
        (m.call != 0).then_some(m)
    }

    pub fn machine<'scope>(store: &mut WasmStore<'_, 'scope>, entry: u64, instance: &'scope WasmInstance<'scope>, code: &Code, fp: usize) -> Result<(), WasmTrap> {
        let (params, results) = (code.params as usize, code.results as usize);
        store.reserve(fp + results);
        WasmJit::run(store, entry, instance, fp, [params, results])
    }

    pub fn osr<'scope>(store: &mut WasmStore<'_, 'scope>, frames: &mut Vec<Activation<'scope>>) -> Result<(), WasmTrap> {
        let Some(f) = frames.last() else { return Ok(()) };
        let Some(m) = WasmJit::looped(store, &f.code) else { return Ok(()) };
        let (instance, fp, n) = (f.instance, f.fp, f.code.registers as usize);
        let results = f.code.results as usize;
        store.ctx.osr = u64::try_from(f.pc).unwrap_or(u64::MAX);
        store.reserve(fp + results);
        WasmJit::run(store, m.osr, instance, fp, [n, results])?;
        frames.pop();
        Ok(())
    }

    fn run<'scope>(store: &mut WasmStore<'_, 'scope>, entry: u64, instance: &'scope WasmInstance<'scope>, fp: usize, [inputs, outputs]: [usize; 2]) -> Result<(), WasmTrap> {
        let width = inputs.max(outputs);
        let mut inline = [0u64; WasmCtx::INLINE_IO];
        let mut heap = Vec::new();
        let io: &mut [u64] = if width <= WasmCtx::INLINE_IO {
            &mut inline[..width]
        } else {
            heap.resize(width, 0);
            &mut heap
        };
        io[..inputs].copy_from_slice(&store.stack[fp..fp + inputs]);
        let caller = store.ctx.instance;
        store.ctx.store = core::ptr::from_mut(store) as usize;
        store.ctx.status = WasmCtx::RETURNED;
        store.ctx.enter(instance);
        store.ctx.depth += 1;
        let run: Entry = unsafe { core::mem::transmute::<usize, Entry>(usize::try_from(entry).unwrap_or(0)) };
        let ctx = core::ptr::from_mut(&mut *store.ctx);
        let status = unsafe { run(ctx, io.as_mut_ptr()) };
        store.ctx.depth -= 1;
        store.ctx.instance = caller;
        store.ctx.refresh();
        match status {
            WasmCtx::RETURNED => {
                store.stack[fp..fp + outputs].copy_from_slice(&io[..outputs]);
                Ok(())
            }
            WasmCtx::THREW => Err(WasmTrap::Exception),
            _ => Err(WasmTrap::from(store.ctx.trap)),
        }
    }
}
