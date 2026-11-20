//! What a JIT tier sees of the executor: a loaded function's decoded bytecode and the feedback its inline caches
//! gathered ([`Function`]), the frame a call or a hot loop is about to run ([`Entry`]), and how compiled code hands an
//! execution back to the interpreter ([`DeoptFrame`]: the interpreter frames its state stands for).
//!
//! ```text
//! a call enters a frame / a loop back edge (every CHECK_STEPS)   ⇒ Tier::enter / Tier::osr with an Entry
//! the tier runs machine code on the frame                        ⇒ Entry::resume: the interpreter frames to continue
//!                                                                   (the entered one, plus inlined callees on top)
//! machine code called from machine code bails out                ⇒ Executor::resume_frames: a nested execution
//! ```
use alloc::sync::Arc;
use alloc::vec::Vec;
use core::cell::Cell;

use j8bytecode::{Bytecode, Constant, OperandValue, SharedFunction};
use j8framework::{Agent, Closure, Error, FuncId, JsEnv, JsObject, Shape, Value};

pub use crate::ic::{GlobalFeedback, PropertyFeedback};
use crate::{Args, Budget, Executor, Frame, Loaded, State, STACK_LIMIT};

/// A tier faster than interpretation (the JIT), handed to [`Executor::with_tier`].
pub trait Tier {
    /// A call is about to run the entry frame from its first instruction
    fn enter(&self, e: &mut Entry<'_, '_, '_, '_>) -> Tiered;
    /// The entry frame's loop back edge at instruction `at` (a `JumpLoop`) has been taken many times
    fn osr(&self, e: &mut Entry<'_, '_, '_, '_>, at: usize) -> Tiered;
    /// The entry frame continues at instruction `pc` (after a pause, or a hand-over from machine code) in a
    /// function the tier knows: it may go on in machine code compiled for a loop with that header
    fn resume(&self, e: &mut Entry<'_, '_, '_, '_>, pc: usize) -> Tiered;
    /// Asked on every call (testing: compile everything at once), not just every few
    fn eager(&self) -> bool {
        false
    }

    /// An ordinary call of `f` (a function the tier knows) without an interpreter frame: `None` to call it the
    /// usual way
    fn call<'gc>(&self, exe: &Executor, a: &mut Agent<'gc, '_>, f: FuncId, callee: Value<'gc>, this: Value<'gc>, args: &[Value<'gc>]) -> Option<Result<Value<'gc>, Error>>;
}

/// The tier's answer.
#[derive(Debug)]
pub enum Tiered {
    /// Not compiled (yet): interpret
    Interpret,
    /// Ran: the frames were set with [`Entry::resume`]; the interpreter continues with the top one
    Resumed,
    /// Ran and threw (the frames set with [`Entry::resume`] first, the throwing one on top at the throwing
    /// instruction)
    Threw(Error),
}

/// One decoded instruction.
#[derive(Clone, Copy, Debug)]
pub struct Insn<'e> {
    pub bytecode: Bytecode,
    /// Register operands are already mapped, see [`Function::reg`]
    pub operands: &'e [OperandValue],
    /// A jump's target (an instruction index)
    pub target: usize,
    /// The byte offset (handler tables count in these)
    pub start: u32,
}

/// What a register operand names.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Reg {
    /// The receiver (0) and the formal parameters (1 …)
    Param(usize),
    /// r`i`
    Local(usize),
    /// The frame's current context
    Context,
    /// The function being run
    Closure,
    /// Another fixed frame slot (rare: the JIT doesn't compile code using one)
    Other,
}

/// Where an interpreter frame keeps its fixed slots: this many values below r0.
pub struct FrameSlots;

impl FrameSlots {
    pub const CONTEXT: usize = Frame::CONTEXT;
    pub const CLOSURE: usize = Frame::CLOSURE;
    pub const NEW_TARGET: usize = Frame::NEW_TARGET;
    pub const NEW_THIS: usize = Frame::NEW_THIS;
}

/// How a context made by `CreateFunctionContext` / `CreateBlockContext` looks.
#[derive(Clone, Copy, Debug)]
pub struct ScopeLayout {
    pub shape: j8framework::ShapeId,
    pub slots: usize,
}

/// A loaded function, read-only.
#[derive(Clone, Copy)]
pub struct Function<'e>(&'e Loaded);

impl<'e> Function<'e> {
    pub fn id(self) -> FuncId {
        self.0.id
    }

    pub fn shared(self) -> &'e SharedFunction {
        &self.0.func
    }

    /// The decoded instructions (the last one is a sentinel `Illegal`)
    pub fn len(self) -> usize {
        self.0.insns.len()
    }

    pub fn is_empty(self) -> bool {
        self.0.insns.is_empty()
    }

    pub fn insn(self, i: usize) -> Insn<'e> {
        let d = &self.0.insns[i];
        Insn { bytecode: d.bytecode, operands: &d.operands.all()[..d.bytecode.operand_types().len()], target: d.target as usize, start: d.start }
    }

    /// The instruction at byte offset `byte`
    pub fn index_of(self, byte: usize) -> usize {
        self.0.index_of(byte)
    }

    /// What decoded register operand `k` names
    pub fn reg(self, k: OperandValue) -> Reg {
        let header = OperandValue::try_from(Frame::HEADER).unwrap_or(0);
        if k > Loaded::SPECIAL_LIMIT {
            if k >= 0 {
                return Reg::Local(usize::try_from(k).unwrap_or(0));
            }
            if k == -OperandValue::try_from(Frame::CONTEXT).unwrap_or(0) {
                return Reg::Context;
            }
            if k == -OperandValue::try_from(Frame::CLOSURE).unwrap_or(0) {
                return Reg::Closure;
            }
            let params = OperandValue::from(self.0.func.bytecode.parameter_count);
            let p = k + params + header;
            return match usize::try_from(p) {
                Ok(p) if k < -header => Reg::Param(p),
                _ => Reg::Other,
            };
        }
        Reg::Other
    }

    pub fn constant(self, k: OperandValue) -> Option<&'e Constant> {
        self.0.func.bytecode.constants.get(usize::try_from(k).ok()?)
    }

    /// A name constant
    pub fn name(self, k: OperandValue) -> &'e str {
        Executor::name_at(self.0, k)
    }

    pub fn property(self, slot: OperandValue) -> Option<PropertyFeedback> {
        self.0.ics.get(usize::try_from(slot).ok()?)?.feedback()
    }

    pub fn global(self, slot: OperandValue) -> Option<GlobalFeedback> {
        self.0.globals.get(usize::try_from(slot).ok()?)?.feedback()
    }

    /// The one function a call / construct site has called, if only one
    pub fn call_target(self, slot: OperandValue) -> Option<FuncId> {
        match self.0.call_ics.get(usize::try_from(slot).ok()?)?.get() {
            0 | u32::MAX => None,
            f => Some(FuncId(f - 1)),
        }
    }

    /// The shape an object literal (boilerplate constant `k`) gets, once seen
    pub fn literal_shape(self, k: OperandValue) -> Option<Arc<Shape>> {
        self.0.literals.get(usize::try_from(k).ok()?)?.borrow().clone()
    }

    /// The context layout of `Scope` constant `k`
    pub fn scope(self, k: OperandValue) -> Option<ScopeLayout> {
        let (shape, n) = self.0.scope_shape(k)?;
        Some(ScopeLayout { shape, slots: n as usize })
    }

    /// The function context's shape, when it has one
    pub fn function_scope(self) -> Option<j8framework::ShapeId> {
        self.0.scope
    }

    /// `new.target.prototype` as the constructor's cache saw it
    pub fn prototype(self) -> Option<PropertyFeedback> {
        self.0.prototype.feedback()
    }

    /// Properties `this` had last time this constructor finished
    pub fn instance_size(self) -> usize {
        usize::from(self.0.instance_size.get())
    }

    /// An ordinary function (not a class constructor, generator or async function), with no function context
    /// extension, no `arguments` / rest parameter
    pub fn plain(self) -> bool {
        let l = self.0;
        !(l.class_constructor || l.generator || l.asynchronous || l.keeps_args || l.extension)
    }

    pub fn constructs(self) -> bool {
        self.0.constructs
    }

    pub fn class_constructor(self) -> bool {
        self.0.class_constructor
    }

    pub fn derived(self) -> bool {
        self.0.derived
    }

    /// A sloppy non-arrow function (its receiver is wrapped)
    pub fn wraps_this(self) -> bool {
        self.0.wraps_this
    }

    pub fn strict(self) -> bool {
        self.0.func.strict
    }

    /// The tier's own word for this function (0 until it sets one)
    pub fn tier_word(self) -> &'e Cell<usize> {
        &self.0.tier
    }
}

/// An interpreter frame the compiled code's state stands for when it hands over. For the entry frame, `callee`,
/// `this` and `args` are ignored (the frame exists); `this` / `args` still overwrite the parameters (code may assign
/// them).
pub struct DeoptFrame<'gc> {
    pub func: FuncId,
    /// The function object (its closure gives the code and the context it was made in)
    pub callee: Value<'gc>,
    /// Receiver, then the formal parameters
    pub params: Vec<Value<'gc>>,
    /// The arguments as passed (only `argc` matters for frames that don't keep them)
    pub argc: usize,
    /// r0 …
    pub regs: Vec<Value<'gc>>,
    /// `Uninit`: the closure's own context
    pub context: Value<'gc>,
    pub acc: Value<'gc>,
    /// The instruction to go on with
    pub pc: usize,
    /// A construct frame: `new.target` and the `this` built (`None`: a plain call)
    pub construct: Option<(Value<'gc>, Value<'gc>)>,
}

/// The frame a call or a hot loop is about to run, with what running it needs.
#[expect(private_interfaces, reason = "built by the executor only")]
pub struct Entry<'r, 'gc, 'x, 'b> {
    pub exe: &'r Executor,
    pub a: &'r mut Agent<'gc, 'x>,
    pub st: &'r mut State<'gc>,
    pub budget: &'r mut Budget<'b>,
    /// Call depth below the entry frame
    pub level: usize,
}

impl<'r, 'gc, 'x, 'b> Entry<'r, 'gc, 'x, 'b> {
    pub fn executor(&self) -> &'r Executor {
        self.exe
    }

    pub fn agent(&mut self) -> &mut Agent<'gc, 'x> {
        self.a
    }

    /// The entry frame's function
    pub fn function(&self) -> Function<'r> {
        // SAFETY: the frame runs on this executor, which outlives the entry
        Function(unsafe { self.st.f().loaded.get() })
    }

    /// The entry frame's r0 (the receiver and parameters are below it, see [`Function::reg`]; the header between)
    pub fn r0(&mut self) -> *mut Value<'gc> {
        let base = self.st.f().base;
        self.st.regs.as_mut_ptr().wrapping_add(base)
    }

    /// The entry frame's receiver (the parameters follow it)
    pub fn params(&mut self) -> *mut Value<'gc> {
        let at = self.st.f().params;
        self.st.regs.as_mut_ptr().wrapping_add(at)
    }

    /// The entry frame is a construct call's
    pub fn constructing(&self) -> bool {
        let f = self.st.f();
        self.st.regs[f.base - Frame::NEW_TARGET].bits() != j8framework::ValueBits::UNDEFINED
    }

    /// The function object the entry frame runs
    pub fn callee(&self) -> Value<'gc> {
        let f = self.st.f();
        self.st.regs[f.base - Frame::CLOSURE]
    }

    /// Arguments passed to the entry frame
    pub fn argc(&self) -> usize {
        self.st.f().argc
    }

    pub fn acc(&self) -> Value<'gc> {
        self.st.acc
    }

    /// Call depth of the entry frame
    pub fn depth(&self) -> usize {
        self.level
    }

    /// Takes `n` loop iterations off the budget; `true` when the execution must stop (out of budget, or terminated)
    pub fn spend(&mut self, n: usize) -> bool {
        let b = &mut *self.budget;
        if self.a.stop.observe() {
            b.terminated = true;
            return true;
        }
        if n <= b.tick {
            b.tick -= n;
            return false;
        }
        let mut left = n - b.tick;
        b.tick = 0;
        while left > 0 {
            if b.refill() {
                return true;
            }
            let t = left.min(b.tick + 1);
            b.tick -= t - 1;
            left -= t;
        }
        false
    }

    /// The entry frame returns `acc` by its `Return` instruction at `pc` (the interpreter runs that instruction)
    pub fn resume_at(&mut self, pc: usize, acc: Value<'gc>) {
        let top = self.st.f_mut();
        top.pc = pc;
        top.insn = pc;
        self.st.acc = acc;
    }

    /// Hands the execution back: `frames[0]` is the entry frame's state, the rest inlined callees, innermost last.
    pub fn resume(&mut self, frames: Vec<DeoptFrame<'gc>>) {
        let mut it = frames.into_iter();
        if let Some(first) = it.next() {
            let f = *self.st.f();
            let regs = &mut self.st.regs;
            for (i, v) in first.params.iter().enumerate() {
                regs[f.params + i] = *v;
            }
            for (i, v) in first.regs.iter().enumerate() {
                regs[f.base + i] = *v;
            }
            if first.context.bits() != j8framework::ValueBits::UNINIT {
                regs[f.base - Frame::CONTEXT] = first.context;
            }
            if let Some((nt, this)) = first.construct {
                regs[f.base - Frame::NEW_TARGET] = nt;
                regs[f.base - Frame::NEW_THIS] = this;
            }
            let top = self.st.f_mut();
            top.pc = first.pc;
            top.insn = first.pc;
            self.st.acc = first.acc;
        }
        for d in it {
            self.exe.push_deopt(self.a, self.st, d);
        }
    }
}

impl Executor {
    /// A loaded function, for the tier
    pub fn jit_function(&self, f: FuncId) -> Function<'_> {
        Function(self.function(f))
    }

    /// The function a closure runs
    pub fn jit_closure_function(&self, c: &Closure<'_>) -> Function<'_> {
        Function(self.code(c))
    }

    /// Pushes interpreter frame `d` (its caller below must have `pc` past the call)
    fn push_deopt<'gc>(&self, a: &mut Agent<'gc, '_>, st: &mut State<'gc>, d: DeoptFrame<'gc>) {
        let Some(c) = d.callee.as_object().and_then(|o| o.closure()) else { unreachable!("a deopt frame of a closure") };
        let l = self.code(&c);
        let args: Vec<Value<'gc>> = d.params.iter().skip(1).copied().chain(core::iter::repeat(Value::Undefined)).take(d.argc.max(d.params.len().saturating_sub(1))).collect();
        let this = d.params.first().copied().unwrap_or_default();
        let (params, base) = self.open_regs(a, &mut st.regs, l, c, d.callee, this, Args::Values(&args));
        // The receiver as the frame had it (already wrapped)
        st.regs[params] = this;
        for (i, v) in d.regs.iter().enumerate() {
            st.regs[base + i] = *v;
        }
        // `Uninit`: the closure's own context (as opened)
        if d.context.bits() != j8framework::ValueBits::UNINIT {
            st.regs[base - Frame::CONTEXT] = d.context;
        }
        if let Some((nt, this)) = d.construct {
            st.regs[base - Frame::NEW_TARGET] = nt;
            st.regs[base - Frame::NEW_THIS] = this;
        }
        st.frames.push(Frame { func: d.func, loaded: crate::LoadedRef::from(l), pc: d.pc, insn: d.pc, params, base, argc: d.argc });
        st.acc = d.acc;
    }

    /// Runs `frames` (outermost first) in a nested execution until the outermost returns; `throw`: the innermost
    /// frame throws it at its `pc`.
    pub fn resume_frames<'gc>(&self, a: &mut Agent<'gc, '_>, frames: Vec<DeoptFrame<'gc>>, throw: Option<Value<'gc>>) -> Result<Value<'gc>, Error> {
        let level = self.depth.get() + 1;
        if level >= STACK_LIMIT {
            return Err(a.throw(j8framework::ErrorKind::Range, "Maximum call stack size exceeded"));
        }
        let mut regs = a.take_regs();
        regs.clear();
        let mut st = State { regs, frames: self.spare_frames.borrow_mut().pop().unwrap_or_default(), acc: Value::Undefined, message: Value::Undefined };
        st.frames.clear();
        for d in frames {
            self.push_deopt(a, &mut st, d);
        }
        if let Some(v) = throw
            && !self.unwind(&mut st, v)
        {
            self.recycle(a, st);
            return Err(a.throw_value(v));
        }
        let saved = self.depth.replace(level);
        let r = self.execute(a, st, &mut None).map(crate::Outcome::returned);
        self.depth.set(saved);
        r
    }

    /// `CreateClosure` of function constant `k` of `f`, in context `env`
    pub fn jit_create_closure<'gc>(&self, a: &Agent<'gc, '_>, f: FuncId, k: OperandValue, env: Option<JsEnv<'gc>>) -> JsObject<'gc> {
        self.create_closure(a, self.function(f), k, env)
    }

    /// `CreateObjectLiteral` of boilerplate constant `k` of `f`
    pub fn jit_object_literal<'gc>(&self, a: &Agent<'gc, '_>, f: FuncId, k: OperandValue) -> JsObject<'gc> {
        Self::object_literal(a, self.function(f), k)
    }

    /// Builds `this` for a construct call of `ctor` (`new.target.prototype` as the prototype)
    pub fn jit_create_this<'gc>(&self, a: &mut Agent<'gc, '_>, ctor: FuncId, new_target: Value<'gc>) -> Result<JsObject<'gc>, Error> {
        Self::create_this(a, self.function(ctor), new_target)
    }

    /// `LdaGlobal` of name constant `k` with feedback slot `slot` of `f`, the interpreter's way
    pub fn jit_global_load<'gc>(&self, a: &mut Agent<'gc, '_>, f: FuncId, k: OperandValue, slot: OperandValue) -> Result<Value<'gc>, Error> {
        let l = self.function(f);
        let name = Self::name_at(l, k);
        let ic = l.globals.get(usize::try_from(slot).unwrap_or(usize::MAX)).ok_or_else(|| a.throw(j8framework::ErrorKind::Reference, "no global cache"))?;
        self.global_load(a, ic, name, false)
    }

    /// Records the size of an instance a constructor built (see `Loaded::instance_size`)
    pub fn jit_instance_size(&self, ctor: FuncId, n: usize) {
        self.function(ctor).instance_size.set(u8::try_from(n).unwrap_or(u8::MAX));
    }

    /// Current call depth (the frames below the running machine code)
    pub fn jit_depth(&self) -> &Cell<usize> {
        &self.depth
    }
}
