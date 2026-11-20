//! j8jit: the JIT for j8 (x86-64 Unix).
//!
//! ```text
//! hot function / hot loop ─ build (bytecode + inline caches ⇒ speculative graph) ─ lower (MLIR llvm dialect)
//!   ─ mlirpass (mem2reg, canonicalize, cse) ─ x86-64 backend ─▶ machine code, run on the interpreter's frame
//! a guard fails ─▶ j8_deopt rebuilds the interpreter frames; the hint it leaves shapes the next compilation
//! ```
//!
//! Three entries: [`Kind::Enter`] (a call from the interpreter, on its frame), [`Kind::Osr`] (a hot loop, on its
//! frame), [`Kind::Native`] (a call from compiled code, through the callee's trampoline, no interpreter frame).
extern crate alloc;

mod build;
mod asm;
mod cfg;
mod codegen;
mod compile;
mod helper;
mod ir;
mod opt;
mod runtime;
mod x86_64;

pub use build::Kind;
pub use compile::{Backend, CompileError, Compiler, MachineCode};
pub use helper::Helper as NumberHelper;
pub use x86_64::{X86_64Code, X86_64Emit, X86_64Reloc};

use alloc::boxed::Box;
use alloc::collections::BTreeMap;
use alloc::rc::Rc;
use alloc::sync::Arc;
use alloc::vec::Vec;
use core::cell::RefCell;

use j8executor::{Entry, Tier, Tiered};
use j8framework::{FuncId, Shape, Value};

use build::{Builder, Hints, HINT_FLOAT, HINT_GENERIC, HINT_NO_INLINE};
use ir::{FrameState, Helper, Op, Rep, Slot, V};
use codegen::{Reason, RESUMED, RETURNED, THREW};
use runtime::{Ctx, Mapped, Trampolines};

/// Where a deopt slot's bits are.
#[derive(Clone, Copy, Debug)]
pub enum Src {
    Buf(u32),
    Const(u64),
}

/// One hand-over point of some compiled code.
pub struct Deopt {
    state: FrameState,
    values: BTreeMap<V, (Rep, Src)>,
    reason: Reason,
    /// The guarding instruction (debugging)
    op: u32,
    /// Where a failure's hint goes, when not at the state's innermost frame (a guard moved out of a loop)
    hint_at: Option<(FuncId, u32)>,
    /// The code was entered natively (no interpreter frame below)
    native: bool,
    /// The compiled function and how it was entered (to drop the code when a speculation fails)
    root: FuncId,
    kind: Kind,
}

/// What the JIT knows about one function.
#[derive(Default)]
struct Func {
    enter: Option<usize>,
    osr: BTreeMap<usize, usize>,
    native: Option<usize>,
    /// Its trampoline (0: none yet)
    stub: usize,
    /// Calls through the trampoline while interpreted
    native_calls: u32,
    deopts: u32,
    never: bool,
    native_never: bool,
}

/// A global cache site: where the name is, the function it is expected to hold, and its two-word cache
struct Site {
    func: FuncId,
    name: u32,
    slot: u32,
    expect: Option<FuncId>,
    /// Its cache (the code reads it by address: kept here)
    #[expect(dead_code, reason = "owned for its address only")]
    cache: Box<[u64; 2]>,
}

#[derive(Default)]
struct State {
    sites: Vec<Site>,
    funcs: Vec<(FuncId, Func)>,
    deopts: Vec<Rc<Deopt>>,
    calls: Vec<(Helper, usize)>,
    shapes: Vec<Option<Arc<Shape>>>,
    keep: Vec<Arc<Shape>>,
    code: Vec<Mapped>,
    tramps: Vec<Trampolines>,
    hints: Hints,
}

/// ⭐ The JIT: handed to the executor (`Executor::with_tier`).
pub struct Jit {
    threshold: u32,
    /// Contexts of finished runs, for the next ones (a host function calling back into JS starts one each time)
    ctxs: RefCell<Vec<Box<Ctx>>>,
    /// The lowest stack address native code runs at (set the first time it runs)
    floor: core::cell::Cell<u64>,
    epoch: core::cell::Cell<u64>,
    st: RefCell<State>,
}

impl Default for Jit {
    fn default() -> Self {
        Self::from(Jit::THRESHOLD)
    }
}

impl From<u32> for Jit {
    /// Native code after `threshold` calls through a trampoline
    fn from(threshold: u32) -> Self {
        Self { threshold, ctxs: RefCell::new(Vec::new()), floor: core::cell::Cell::new(0), epoch: core::cell::Cell::new(1), st: RefCell::new(State::default()) }
    }
}

/// Deopts of one function before the JIT gives up on it
const MAX_DEOPTS: u32 = 24;

/// An interpreter entry
type EntryFn = unsafe extern "C" fn(*mut Ctx, *mut Value<'static>, *mut Value<'static>) -> i64;

impl Jit {
    /// Default hotness threshold
    pub const THRESHOLD: u32 = 64;

    pub fn threshold(&self) -> u32 {
        self.threshold
    }

    /// Helper call `id`: what and how many operands
    fn call(&self, id: u64) -> (Helper, usize) {
        self.st.borrow().calls[usize::try_from(id).unwrap_or(0)]
    }

    fn spare_ctx(&self) -> Option<Box<Ctx>> {
        self.ctxs.borrow_mut().pop()
    }

    /// Keeps a context for reuse
    fn recycle(&self, ctx: Box<Ctx>) {
        let mut v = self.ctxs.borrow_mut();
        if v.len() < 8 {
            v.push(ctx);
        }
    }

    /// See `Ctx::stack_floor`
    fn stack_floor(&self) -> u64 {
        if self.floor.get() == 0 {
            let here = 0u8;
            let at = core::ptr::from_ref(&here) as u64;
            self.floor.set(at.saturating_sub(runtime::NATIVE_STACK));
        }
        self.floor.get()
    }

    /// The machine stack has room for native code (else the interpreter goes on with its own frames)
    fn stack_room(&self) -> bool {
        let here = 0u8;
        let at = core::ptr::from_ref(&here) as u64;
        at > self.stack_floor() + runtime::NATIVE_STACK / 4
    }

    /// A new epoch (see `Ctx::epoch`)
    fn bump(&self) -> u64 {
        let e = self.epoch.get() + 1;
        self.epoch.set(e);
        e
    }

    /// Global cache site `k`: (function, name constant, feedback slot, expected function)
    fn site(&self, k: u32) -> (FuncId, u32, u32, Option<FuncId>) {
        let st = self.st.borrow();
        let s = &st.sites[k as usize];
        (s.func, s.name, s.slot, s.expect)
    }

    fn shape(&self, k: u32) -> Option<Arc<Shape>> {
        self.st.borrow().shapes.get(k as usize).cloned().flatten()
    }

    fn deopt(&self, id: u64) -> Rc<Deopt> {
        self.st.borrow().deopts[usize::try_from(id).unwrap_or(0)].clone()
    }

    fn func<'s>(st: &'s mut State, f: FuncId) -> &'s mut Func {
        let i = match st.funcs.iter().position(|(x, _)| *x == f) {
            Some(i) => i,
            None => {
                st.funcs.push((f, Func::default()));
                st.funcs.len() - 1
            }
        };
        &mut st.funcs[i].1
    }

    /// A failed speculation: a hint for next time, and the code that made it goes
    fn learn(&self, d: &Deopt) {
        let Some(fr) = d.state.frames.last() else { return };
        if std::env::var_os("J8_JIT_LOG").is_some() {
            std::eprintln!("j8jit: deopt {:?} {:?} at {:?}@{} ({:?}, v{})", d.root, d.kind, fr.func, fr.pc, d.reason, d.op);
        }
        let key = d.hint_at.map_or((fr.func.0, fr.pc), |(f, pc)| (f.0, pc));
        let bits = match d.reason {
            Reason::Int => HINT_FLOAT,
            Reason::Number | Reason::Shape => HINT_GENERIC,
            Reason::Func => HINT_NO_INLINE | HINT_GENERIC,
            Reason::Always => 0,
            Reason::Poll | Reason::Throw | Reason::Hole | Reason::Depth => return,
        };
        let mut st = self.st.borrow_mut();
        *st.hints.entry(key).or_default() |= bits;
        let native_stub;
        {
            let f = Self::func(&mut st, d.root);
            f.deopts += 1;
            if f.deopts >= MAX_DEOPTS {
                f.never = true;
                f.native_never = true;
            }
            match d.kind {
                Kind::Enter => f.enter = None,
                Kind::Osr(at) => {
                    f.osr.remove(&at);
                }
                Kind::Native(_) => {
                    f.native = None;
                    f.native_calls = 0;
                }
            }
            native_stub = (matches!(d.kind, Kind::Native(_)), f.stub);
        }
        if native_stub.0 && native_stub.1 != 0 {
            Self::point(&st, native_stub.1, runtime::j8_interp as *const () as usize);
        }
    }

    fn point(st: &State, stub: usize, target: usize) {
        if let Some(t) = st.tramps.iter().find(|t| t.owns(stub)) {
            t.set_at(stub, target);
        }
    }

    /// The trampoline of `f` (made pointing at the interpreter)
    fn stub(st: &mut State, f: FuncId) -> usize {
        let s = Self::func(st, f).stub;
        if s != 0 {
            return s;
        }
        let interp = runtime::j8_interp as *const () as usize;
        let mut s = st.tramps.last_mut().and_then(|t| t.take(interp));
        if s.is_none() {
            if let Some(mut t) = Trampolines::new() {
                s = t.take(interp);
                st.tramps.push(t);
            }
        }
        let s = s.unwrap_or(0);
        Self::func(st, f).stub = s;
        s
    }

    /// A call through `f`'s trampoline was interpreted: compiles native code when hot
    fn native_hot(&self, exe: &j8executor::Executor, a: &mut j8framework::Agent<'_, '_>, f: FuncId) {
        {
            let mut st = self.st.borrow_mut();
            let fs = Self::func(&mut st, f);
            if fs.native_never || fs.native.is_some() {
                return;
            }
            fs.native_calls += 1;
            if fs.native_calls < self.threshold {
                return;
            }
        }
        let formals = usize::from(exe.jit_function(f).shared().formal_parameters).min(3);
        match self.compile(exe, a, f, Kind::Native(formals)) {
            Some(addr) => {
                let mut st = self.st.borrow_mut();
                Self::func(&mut st, f).native = Some(addr);
                let stub = Self::stub(&mut st, f);
                Self::point(&st, stub, addr);
            }
            None => Self::func(&mut self.st.borrow_mut(), f).native_never = true,
        }
    }

    /// Compiles `f` entered as `kind`: the machine code's address
    fn compile(&self, exe: &j8executor::Executor, a: &mut j8framework::Agent<'_, '_>, f: FuncId, kind: Kind) -> Option<usize> {
        if std::env::var_os("J8_JIT_LOG").is_some() {
            std::eprintln!("j8jit: compile {f:?} {kind:?}");
        }
        let t0 = std::time::Instant::now();
        let hints = self.st.borrow().hints.clone();
        let (global, scope) = (a.realm.global, a.realm.script_scope);
        let paths = move |g: bool, i: usize| if g { global.get().props().store_path(i) } else { scope.get().props().store_path(i) };
        let built = Builder::build(exe, &hints, f, kind, &paths).ok()?;
        let (deopt_base, helper_base) = {
            let st = self.st.borrow();
            (u32::try_from(st.deopts.len()).ok()?, u32::try_from(st.calls.len()).ok()?)
        };
        let t_build = t0.elapsed();
        if std::env::var_os("J8_JIT_IR").is_some() {
            std::eprintln!("== {f:?} {kind:?}\n{}", built.graph.dump());
        }
        // Global cache sites: their caches first (the code embeds their addresses), numbered globally
        let mut graph = built.graph;
        let (site_base, addrs) = {
            let mut st = self.st.borrow_mut();
            let base = u32::try_from(st.sites.len()).ok()?;
            let mut expects = alloc::vec![None; graph.globals.len()];
            for inst in &mut graph.insts {
                if let Op::Global(k, e, _) = &mut inst.op {
                    expects[*k as usize] = *e;
                    *k += base;
                }
            }
            let mut addrs = Vec::new();
            for (i, (f, name, slot)) in graph.globals.iter().enumerate() {
                let cache = Box::new([0u64; 2]);
                addrs.push(cache.as_ptr() as usize as u64);
                st.sites.push(Site { func: *f, name: *name, slot: *slot, expect: expects[i], cache });
            }
            (base, addrs)
        };
        let mut all_addrs = alloc::vec![0u64; site_base as usize];
        all_addrs.extend(addrs);
        let built = build::Built { graph, ..built };
        let generated = codegen::Gen::generate(&built.graph, kind, deopt_base, helper_base, &all_addrs);
        if std::env::var_os("J8_JIT_LOG").is_some() {
            std::eprintln!("j8jit: {f:?} {kind:?}: {} insts, {} bytes, build {t_build:?} total {:?}", built.graph.insts.len(), generated.bytes.len(), t0.elapsed());
        }
        if let Some(dir) = std::env::var_os("J8_JIT_BIN") {
            let mut path = std::path::PathBuf::from(dir);
            path.push(alloc::format!("f{}-{kind:?}.bin", f.0));
            let _ = std::fs::write(path, &generated.bytes);
        }
        let mut st = self.st.borrow_mut();
        let mut targets = Vec::new();
        for t in &built.targets {
            targets.push(Self::stub(&mut st, *t));
        }
        let sym = |s: codegen::Sym| -> usize {
            match s {
                codegen::Sym::Deopt => runtime::j8_deopt as *const () as usize,
                codegen::Sym::Poll => runtime::j8_poll as *const () as usize,
                codegen::Sym::Helper => runtime::j8_helper as *const () as usize,
                codegen::Sym::Target(k) => targets[k as usize],
            }
        };
        let mapped = Mapped::new(&generated.bytes, |buf, _| {
            for (off, s) in &generated.relocs {
                buf[*off..*off + 8].copy_from_slice(&(sym(*s) as u64).to_le_bytes());
            }
        })?;
        let lowered = generated;
        let addr = mapped.at as usize;
        st.code.push(mapped);
        // Runtime calls: shapes get global indices
        let shape_base = u32::try_from(st.shapes.len()).ok()?;
        st.shapes.extend(built.shapes.iter().cloned());
        st.keep.extend(built.keep.iter().cloned());
        let arity = &lowered.arity;
        for (i, h) in lowered.calls.iter().enumerate() {
            let h = match h {
                Helper::Alloc(k) => Helper::Alloc(k + shape_base),
                h => *h,
            };
            st.calls.push((h, arity[i]));
        }
        let native = matches!(kind, Kind::Native(_));
        for (i, exit) in lowered.exits.iter().enumerate() {
            let state = built.graph.states[exit.fs as usize].clone();
            let mut values = BTreeMap::new();
            for (v, slot) in &lowered.values[i] {
                values.insert(*v, (built.graph.rep(*v), Src::Buf(*slot)));
            }
            for v in Self::state_values(&state) {
                if values.contains_key(&v) {
                    continue;
                }
                let rep = built.graph.rep(v);
                let bits = match built.graph.op(v) {
                    Op::I32(x) => u64::from(x.cast_unsigned()) | if *x < 0 { 0xFFFF_FFFF_0000_0000 } else { 0 },
                    Op::F64(x) => x.to_bits(),
                    Op::Word(w) => *w,
                    _ => j8framework::ValueBits::UNDEFINED,
                };
                values.insert(v, (rep, Src::Const(bits)));
            }
            let hint_at = exit.hint.and_then(|h| built.graph.states[h as usize].frames.last().map(|fr| (fr.func, fr.pc)));
            st.deopts.push(Rc::new(Deopt { state, values, reason: exit.reason, op: exit.op, hint_at, native, root: f, kind }));
        }
        Some(addr)
    }

    fn state_values(st: &FrameState) -> Vec<V> {
        let mut out = Vec::new();
        let mut add = |s: &Slot| {
            if let Slot::Value(v) = s {
                out.push(*v);
            }
        };
        for fr in &st.frames {
            add(&fr.callee);
            fr.params.iter().for_each(&mut add);
            fr.regs.iter().for_each(&mut add);
            add(&fr.context);
            add(&fr.acc);
            if let Some((a, b)) = &fr.construct {
                add(a);
                add(b);
            }
        }
        for o in &st.objects {
            add(&Slot::Value(o.proto));
            o.fields.iter().for_each(&mut add);
        }
        for (c, _, s) in &st.writes {
            add(&Slot::Value(*c));
            add(s);
        }
        out
    }

    /// Runs native code at `addr` for the entry frame's call; the frame then returns by its `Return` at `ret`
    fn run_native(&self, e: &mut Entry<'_, '_, '_, '_>, addr: usize, ret: usize) -> Tiered {
        let params = e.params();
        // SAFETY: the receiver and the formal parameters (at most three) are in the frame
        let words: [u64; 4] = core::array::from_fn(|i| if i <= e.function().shared().formal_parameters as usize { unsafe { (*params.add(i)).bits() } } else { j8framework::ValueBits::UNDEFINED });
        let callee = e.callee().bits();
        let level = e.depth();
        let exe = e.executor();
        let saved = exe.jit_depth().replace(level);
        let mut ctx = Ctx::new(e, self);
        type NativeFn = unsafe extern "C" fn(*mut Ctx, u64, u64, u64, u64, u64) -> u64;
        // SAFETY: native code takes (context, callee, this, three arguments)
        let r = unsafe {
            let f: NativeFn = core::mem::transmute::<usize, NativeFn>(addr);
            f(&raw mut *ctx, callee, words[0], words[1], words[2], words[3])
        };
        exe.jit_depth().set(saved);
        let threw = (ctx.pending != 0).then(|| ctx.error.take().unwrap_or(j8framework::Error::Terminated));
        self.recycle(ctx);
        if let Some(err) = threw {
            return Tiered::Threw(err);
        }
        // SAFETY: a word of this mutate
        e.resume_at(ret, unsafe { Value::from_bits(r) });
        Tiered::Resumed
    }

    /// Runs interpreter-entry code at `addr` on the entry frame
    fn run(&self, e: &mut Entry<'_, '_, '_, '_>, addr: usize) -> Tiered {
        let params = e.params().cast::<Value<'static>>();
        let r0 = e.r0().cast::<Value<'static>>();
        let level = e.depth();
        let exe = e.executor();
        let saved = exe.jit_depth().replace(level);
        let mut ctx = Ctx::new(e, self);
        // SAFETY: the code was compiled for this entry kind; it reads the frame through the pointers, and the
        // context through `ctx`
        let status = unsafe {
            let f: EntryFn = core::mem::transmute::<usize, EntryFn>(addr);
            f(&raw mut *ctx, params, r0)
        };
        exe.jit_depth().set(saved);
        let r = match status {
            RETURNED => {
                // SAFETY: a word of this mutate
                let v = unsafe { Value::from_bits(ctx.ret) };
                e.resume_at(usize::try_from(ctx.ret_pc).unwrap_or(0), v);
                Tiered::Resumed
            }
            RESUMED => Tiered::Resumed,
            THREW => Tiered::Threw(ctx.error.take().unwrap_or(j8framework::Error::Terminated)),
            _ => Tiered::Resumed,
        };
        self.recycle(ctx);
        r
    }
}

impl Tier for Jit {
    fn eager(&self) -> bool {
        self.threshold == 0
    }

    fn call<'gc>(&self, exe: &j8executor::Executor, a: &mut j8framework::Agent<'gc, '_>, f: FuncId, callee: Value<'gc>, this: Value<'gc>, args: &[Value<'gc>]) -> Option<Result<Value<'gc>, j8framework::Error>> {
        let func = exe.jit_function(f);
        let formals = usize::from(func.shared().formal_parameters);
        if formals > 3 || !func.plain() || func.class_constructor() || !self.stack_room() {
            return None;
        }
        let addr = {
            let mut st = self.st.borrow_mut();
            Self::func(&mut st, f).native?
        };
        // A sloppy function's receiver: the global object for undefined / null; other primitives the usual way
        let this = if func.wraps_this() {
            match this.bits() {
                j8framework::ValueBits::UNDEFINED | j8framework::ValueBits::NULL => Value::Object(a.realm.global),
                _ if this.as_object().is_some() => this,
                _ => return None,
            }
        } else {
            this
        };
        let arg = |i: usize| if i < formals { args.get(i).copied().unwrap_or_default().bits() } else { j8framework::ValueBits::UNDEFINED };
        let mut ctx = Ctx::bare(exe, a, self);
        type NativeFn = unsafe extern "C" fn(*mut Ctx, u64, u64, u64, u64, u64) -> u64;
        // SAFETY: native code takes (context, callee, this, three arguments)
        let r = unsafe {
            let code: NativeFn = core::mem::transmute::<usize, NativeFn>(addr);
            code(&raw mut *ctx, callee.bits(), this.bits(), arg(0), arg(1), arg(2))
        };
        let threw = (ctx.pending != 0).then(|| ctx.error.take().unwrap_or(j8framework::Error::Terminated));
        self.recycle(ctx);
        Some(match threw {
            Some(e) => Err(e),
            // SAFETY: a word of this mutate
            None => Ok(unsafe { Value::from_bits(r) }),
        })
    }

    fn enter(&self, e: &mut Entry<'_, '_, '_, '_>) -> Tiered {
        let f = e.function();
        let id = f.id();
        f.tier_word().set(1);
        if !self.stack_room() {
            return Tiered::Interpret;
        }
        // An ordinary call of a plain function: its native code (one compilation serves both kinds of callers)
        let formals = usize::from(f.shared().formal_parameters);
        if f.plain() && !f.class_constructor() && !e.constructing() && formals <= 3 && e.callee().as_object().and_then(|o| o.closure()).is_some() {
            let native = {
                let mut st = self.st.borrow_mut();
                let fs = Self::func(&mut st, id);
                if fs.native_never { None } else { Some(fs.native) }
            };
            if let Some(native) = native {
                let addr = match native {
                    Some(a) => Some(a),
                    None => match self.compile(e.executor(), e.agent(), id, Kind::Native(formals)) {
                        Some(a) => {
                            let mut st = self.st.borrow_mut();
                            Self::func(&mut st, id).native = Some(a);
                            let stub = Self::stub(&mut st, id);
                            Self::point(&st, stub, a);
                            Some(a)
                        }
                        None => {
                            Self::func(&mut self.st.borrow_mut(), id).native_never = true;
                            None
                        }
                    },
                };
                if let Some(addr) = addr
                    && let Some(ret) = (0..f.len()).find(|&i| f.insn(i).bytecode == j8bytecode::Bytecode::Return)
                {
                    return self.run_native(e, addr, ret);
                }
            }
        }
        let code = {
            let mut st = self.st.borrow_mut();
            let fs = Self::func(&mut st, id);
            if fs.never {
                return Tiered::Interpret;
            }
            fs.enter
        };
        let addr = match code {
            Some(a) => a,
            None => match self.compile(e.executor(), e.agent(), id, Kind::Enter) {
                Some(a) => {
                    Self::func(&mut self.st.borrow_mut(), id).enter = Some(a);
                    a
                }
                None => {
                    Self::func(&mut self.st.borrow_mut(), id).never = true;
                    return Tiered::Interpret;
                }
            },
        };
        self.run(e, addr)
    }

    fn resume(&self, e: &mut Entry<'_, '_, '_, '_>, pc: usize) -> Tiered {
        let f = e.function();
        let id = f.id();
        if !self.stack_room() {
            return Tiered::Interpret;
        }
        let code = {
            let mut st = self.st.borrow_mut();
            let fs = Self::func(&mut st, id);
            if fs.never {
                return Tiered::Interpret;
            }
            fs.osr.iter().find(|(at, _)| f.insn(**at).target == pc).map(|(_, a)| *a)
        };
        match code {
            Some(addr) => self.run(e, addr),
            None => Tiered::Interpret,
        }
    }

    fn osr(&self, e: &mut Entry<'_, '_, '_, '_>, at: usize) -> Tiered {
        let f = e.function();
        let id = f.id();
        f.tier_word().set(1);
        if !self.stack_room() {
            return Tiered::Interpret;
        }
        let code = {
            let mut st = self.st.borrow_mut();
            let fs = Self::func(&mut st, id);
            if fs.never {
                return Tiered::Interpret;
            }
            fs.osr.get(&at).copied()
        };
        let addr = match code {
            Some(a) => a,
            None => match self.compile(e.executor(), e.agent(), id, Kind::Osr(at)) {
                Some(a) => {
                    Self::func(&mut self.st.borrow_mut(), id).osr.insert(at, a);
                    a
                }
                None => {
                    Self::func(&mut self.st.borrow_mut(), id).never = true;
                    return Tiered::Interpret;
                }
            },
        };
        self.run(e, addr)
    }
}

// SAFETY: the JIT lives in an executor, which moves between threads with its isolate, one thread at a time; its
// code and trampolines are only run and changed from that thread
unsafe impl Send for Jit {}
