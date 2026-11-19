//! j8executor: runs j8 bytecode, which is V8's Ignition bytecode (see j8bytecode).
//!
//! Explicit frame stack (no host recursion); all frames share one register file. A frame's part of it holds the
//! receiver, the formal parameters and the registers r0 …; register operands are V8's frame slots (r0 is -7, the
//! receiver +2, the context -1, the closure -2) and are mapped onto that. Contexts are [`Env`]s: registers and the
//! accumulator carry them as [`Value::context`] words between `Create*Context` / `PushContext` / `PopContext`.
//! Exceptions look up the bytecode's handler table. An `Executor` serves only one isolate: it holds the loaded
//! functions, and closures on the heap point back to them by [`FuncId`].
#![cfg_attr(tail_calls, feature(explicit_tail_calls))]
#![cfg_attr(tail_calls, expect(incomplete_features, reason = "explicit tail calls are what threaded dispatch needs"))]
extern crate alloc;

use alloc::boxed::Box;
use alloc::sync::Arc;
use alloc::vec::Vec;
use core::cell::{Cell, RefCell};

use j8bytecode::{Bytecode, Constant, Context, Decoded, FunctionKind, Intrinsic, LiteralValue, Module, Operands, OperandValue, Register, Runtime, ScopeInfo, ScopeType, SharedFunction};
use j8compiler::Compile;
use j8framework::{Agent, BinaryOp, Closure, Shape, SmallStr, Collect, CompareOp, ContextKind, ContextName, ContextShape, ShapeId, Env, Error, ErrorKind, FuncId, Gc, Invoke, JsEnv, JsObject, Numbers, ObjectData, ObjectKind, Property, Resume, Slot, Step, Value, ValueKind};
use j8lockfree::SegVec;

mod cache;
mod ic;
mod module;
mod threaded;

pub use cache::CodeCache;

/// Call depth limit (including JS re-entered from host functions); exceeding it throws RangeError
const STACK_LIMIT: usize = 10_000;

/// The name a script's `this` is read by from the functions inside it (`LdaGlobal`)
const SCRIPT_THIS: &str = "this";

/// V8's `DefineKeyedOwnPropertyFlag::kSetFunctionName` (also `DefineKeyedOwnPropertyInLiteralFlag`)
const SET_FUNCTION_NAME: OperandValue = 1;

/// V8's `JSGeneratorObject::kRethrow`: an async generator's yielded value was rejected
const RESUME_RETHROW: f64 = 3.0;

/// Frame records reserved for a new execution (deeper calls grow the stack)
const FRAMES_RESERVED: usize = 16;

/// At most how many frame stacks the executor keeps for reuse
const SPARE_FRAMES: usize = 8;

/// Instructions between checks of the budget / termination request
const CHECK_STEPS: usize = 1024;

/// One instruction of a loaded function, decoded once at load time; jump targets are instruction indices.
struct Insn {
    /// Runs it (threaded code)
    handler: threaded::Handler,
    bytecode: Bytecode,
    operands: Operands,
    /// Byte offset of the bytecode byte (after any prefix): jump tables count from here, and it lies inside the handler
    /// table ranges covering the instruction
    start: u32,
    /// Where a jump goes (its index in the instructions); 0 for other bytecodes
    target: u32,
}

/// A loaded function: the function itself plus the id of the first function of its module (`CreateClosure`'s
/// function constants are indices relative to it), with what running it needs prepared.
struct Loaded {
    base: u32,
    func: SharedFunction,
    /// The bytecode, decoded
    insns: Box<[Insn]>,
    /// By feedback slot: inline caches for property reads / writes
    ics: Box<[ic::Ic]>,
    /// By feedback slot: where a global name was last found
    globals: Box<[ic::GlobalIc]>,
    /// By constant index: for object boilerplates, the shape the literal ends up with (then a literal is made in one
    /// step, as V8 copies the boilerplate with its map)
    literals: Box<[RefCell<Option<Arc<Shape>>>]>,
    /// The function's name, ready to clone into a name string
    name: SmallStr,
    /// The shape of its function objects (`length`, `name`, maybe `prototype`), and of their `prototype` objects
    function_shape: RefCell<Option<Arc<Shape>>>,
    prototype_shape: RefCell<Option<Arc<Shape>>>,
    /// By constant index: the context shapes of `Scope` constants
    shapes: Box<[Option<ShapeId>]>,
    /// The function context's shape (when the function has one) / the script context's
    scope: Option<ShapeId>,
    /// Whether its function context gets an extension object (a sloppy direct eval declares `var`s there)
    extension: bool,
    /// `new` builds `this` and opens a frame (an ordinary function or a base class constructor)
    constructs: bool,
    /// A class constructor (cannot be called without `new`)
    class_constructor: bool,
    derived: bool,
    generator: bool,
    asynchronous: bool,
    /// Arrow functions and methods: no `prototype`, not constructors
    no_prototype: bool,
    /// Sloppy, not an arrow function: an `undefined` / `null` receiver becomes the global object, a primitive one its
    /// wrapper
    wraps_this: bool,
    /// The code reads all arguments (rest parameter / arguments object): frames keep them
    keeps_args: bool,
    /// How many properties the instance constructed last time had: reserve that many slots when building `this` next
    /// time
    instance_size: Cell<u8>,
    /// Inline cache for reading `new.target.prototype` during construction
    prototype: ic::Ic,
}

impl Loaded {
    /// `shapes`: the executor's table of context shapes, where this function's are added
    fn new(base: u32, func: SharedFunction, table: &SegVec<ContextShape>) -> Self {
        let bc = &func.bytecode;
        let ics = bc.feedback.iter().map(|_| ic::Ic::default()).collect();
        let globals = bc.feedback.iter().map(|_| ic::GlobalIc::default()).collect();
        let shapes = bc.constants.iter().map(|c| match c {
            Constant::Scope(s) => Some(Self::add_shape(table, s)),
            _ => None,
        }).collect();
        let literals = bc.constants.iter().map(|_| RefCell::new(None)).collect();
        let scope = func.scope.as_ref().map(|s| Self::add_shape(table, s));
        let extension = func.scope.as_ref().is_some_and(|s| s.has_extension);
        let k = func.kind;
        use FunctionKind as F;
        let generator = matches!(k, F::GeneratorFunction | F::AsyncGeneratorFunction | F::ConciseGeneratorMethod | F::StaticConciseGeneratorMethod | F::AsyncConciseGeneratorMethod | F::StaticAsyncConciseGeneratorMethod);
        let asynchronous = matches!(k, F::AsyncFunction | F::AsyncArrowFunction | F::AsyncConciseMethod | F::StaticAsyncConciseMethod | F::AsyncGeneratorFunction | F::AsyncConciseGeneratorMethod | F::StaticAsyncConciseGeneratorMethod);
        let class_constructor = matches!(k, F::BaseConstructor | F::DerivedConstructor | F::DefaultBaseConstructor | F::DefaultDerivedConstructor);
        let derived = matches!(k, F::DerivedConstructor | F::DefaultDerivedConstructor);
        let normal = matches!(k, F::NormalFunction);
        let insns = Self::decode(bc);
        let keeps_args = insns.iter().any(|d| matches!(d.bytecode, Bytecode::CreateRestParameter | Bytecode::CreateMappedArguments | Bytecode::CreateUnmappedArguments | Bytecode::ConstructForwardAllArgs));
        Self {
            base,
            insns,
            ics,
            globals,
            literals,
            name: SmallStr::from(&*func.name),
            function_shape: RefCell::new(None),
            prototype_shape: RefCell::new(None),
            shapes,
            scope,
            extension,
            constructs: !generator && !asynchronous && (normal || matches!(k, F::BaseConstructor | F::DefaultBaseConstructor)),
            class_constructor,
            derived,
            generator,
            asynchronous,
            no_prototype: !normal && !generator,
            keeps_args,
            wraps_this: !func.strict && !matches!(k, F::ArrowFunction | F::AsyncArrowFunction),
            instance_size: Cell::new(0),
            prototype: ic::Ic::default(),
            func,
        }
    }

    /// Decodes the bytecode, turning jump offsets into instruction indices
    fn decode(bc: &j8bytecode::BytecodeArray) -> Box<[Insn]> {
        let decoded: Vec<Decoded> = j8bytecode::Decoder::from(&bc.code[..]).map(|d| d.expect("compiled bytecode decodes")).collect();
        let index = |byte: usize| u32::try_from(decoded.partition_point(|d| d.offset < byte)).unwrap_or(u32::MAX);
        decoded
            .iter()
            .map(|d| {
                let target = match d.bytecode {
                    Bytecode::JumpLoop => index(d.start() - usize::try_from(d.operands[0]).unwrap_or(0)),
                    b if b.is_jump_constant() => match bc.constants.get(usize::try_from(d.operands[0]).unwrap_or(usize::MAX)) {
                        Some(Constant::Offset(o)) => index(d.start() + *o as usize),
                        _ => 0,
                    },
                    b if b.is_jump() => index(d.start() + usize::try_from(d.operands[0]).unwrap_or(0)),
                    _ => 0,
                };
                let mut operands = d.operands;
                for (v, t) in operands.iter_mut().zip(d.bytecode.operand_types()) {
                    if t.is_register() {
                        *v = Self::register(*v, bc.parameter_count);
                    }
                }
                // A short `Star` becomes `Star` with its register
                let bytecode = if d.bytecode.is_short_star() {
                    operands = Operands::from(&[OperandValue::from(d.bytecode.short_star_register().0)][..]);
                    Bytecode::Star
                } else {
                    d.bytecode
                };
                Insn { handler: Insn::handler(bytecode), bytecode, operands, start: u32::try_from(d.start()).unwrap_or(u32::MAX), target }
            })
            // A sentinel: code never runs past its end
            .chain(core::iter::once(Insn { handler: Insn::handler(Bytecode::Illegal), bytecode: Bytecode::Illegal, operands: Operands::default(), start: u32::try_from(bc.code.len()).unwrap_or(u32::MAX), target: 0 }))
            .collect()
    }

    /// Register operands below this are the frame's fixed slots (context, closure, argc): `SPECIAL` plus the V8 operand
    const SPECIAL: OperandValue = OperandValue::MIN / 2;
    const SPECIAL_LIMIT: OperandValue = Self::SPECIAL / 2;

    /// A register operand as an index relative to r0 in the register file: r`i` is `i`, the receiver and parameters
    /// (just below r0) negative; the fixed frame slots `SPECIAL` plus their operand
    fn register(operand: OperandValue, parameter_count: u16) -> OperandValue {
        let o = i32::try_from(operand).unwrap_or(0);
        if o <= Register::REGISTER_FILE_FROM_FP {
            OperandValue::from(Register::from(o).0)
        } else if o >= Register::FIRST_PARAM_FROM_FP {
            OperandValue::from(o - Register::FIRST_PARAM_FROM_FP) - OperandValue::from(parameter_count) - Self::below(Frame::HEADER)
        } else if o == Register::CONTEXT_FROM_FP {
            -Self::below(Frame::CONTEXT)
        } else if o == Register::FUNCTION_FROM_FP {
            -Self::below(Frame::CLOSURE)
        } else {
            Self::SPECIAL + operand
        }
    }

    /// How far below r0 a header slot is, as a register offset
    fn below(slot: usize) -> OperandValue {
        OperandValue::try_from(slot).unwrap_or(0)
    }

    /// The instruction at byte offset `byte` (handler targets, jump tables)
    fn index_of(&self, byte: usize) -> usize {
        self.insns.partition_point(|d| (d.start as usize) < byte)
    }

    /// A scope info's shape for contexts made from it
    /// Adds a context shape to the executor's table
    fn add_shape(table: &SegVec<ContextShape>, s: &ScopeInfo) -> ShapeId {
        ShapeId(u32::try_from(table.push(Self::shape(s))).expect("fewer than 2^32 context shapes"))
    }

    fn shape(s: &ScopeInfo) -> ContextShape {
        let kind = match s.scope_type {
            Some(ScopeType::FunctionScope) | None => ContextKind::Function,
            Some(ScopeType::CatchScope) => ContextKind::Catch,
            Some(ScopeType::WithScope) => ContextKind::With,
            Some(ScopeType::ScriptScope) => ContextKind::Script,
            Some(ScopeType::EvalScope) => ContextKind::Eval,
            Some(ScopeType::ModuleScope) => ContextKind::Module,
            Some(_) => ContextKind::Block,
        };
        let names = s.variables.iter().map(|v| ContextName {
            name: (&*v.name).into(),
            constant: v.mode == j8bytecode::VariableMode::Const,
            lexical: matches!(v.mode, j8bytecode::VariableMode::Let | j8bytecode::VariableMode::Const),
        }).collect();
        ContextShape { kind, names }
    }
}

/// A call frame's record on the frame stack: where its code and its part of the register file are. What the GC traces
/// lives in the register file, in header slots right below r0 (V8 keeps `<context>` / `<closure>` in its stack frame
/// the same way): see [`Frame::CONTEXT`] etc.
#[derive(Clone, Copy, Collect)]
#[collect(require_static)]
struct Frame {
    func: FuncId,
    /// The next instruction (an index into the function's instructions)
    pc: usize,
    /// The instruction being executed: handler table lookups use it
    insn: usize,
    /// Index of the receiver in the register file (the parameters follow)
    params: usize,
    /// Index of r0 in the register file
    base: usize,
    /// The number of arguments passed
    argc: usize,
}

impl Frame {
    /// Header slots, counted down from r0: the current context (V8's `<context>` register)
    const CONTEXT: usize = 1;
    /// The function being run (V8's `<closure>` register)
    const CLOSURE: usize = 2;
    /// `new.target` (`undefined` for ordinary calls)
    const NEW_TARGET: usize = 3;
    /// `this` of a construct call (the hole when absent): present on entry for base class construction
    const NEW_THIS: usize = 4;
    /// All arguments as an array (kept only by functions that read them all), else `undefined`
    const ARGS: usize = 5;
    /// The header's size: the parameters end this far below r0
    const HEADER: usize = 5;

    #[inline]
    fn slot<'gc>(&self, regs: &[Value<'gc>], k: usize) -> Value<'gc> {
        regs[self.base - k]
    }

    #[inline]
    fn set_slot<'gc>(&self, regs: &mut [Value<'gc>], k: usize, v: Value<'gc>) {
        regs[self.base - k] = v;
    }

    /// The current context
    #[inline]
    fn env<'gc>(&self, regs: &[Value<'gc>]) -> Option<JsEnv<'gc>> {
        self.slot(regs, Self::CONTEXT).as_context()
    }

    #[inline]
    fn set_env<'gc>(&self, regs: &mut [Value<'gc>], e: Option<JsEnv<'gc>>) {
        self.set_slot(regs, Self::CONTEXT, e.map_or(Value::Undefined, Value::context));
    }

    /// `this` of a construct call, if built yet
    fn new_this<'gc>(&self, regs: &[Value<'gc>]) -> Option<Value<'gc>> {
        Some(self.slot(regs, Self::NEW_THIS)).filter(|v| !matches!(v.kind(), ValueKind::Uninit))
    }

    /// All the arguments passed (for functions that keep them)
    fn args<'gc>(&self, regs: &[Value<'gc>]) -> Vec<Value<'gc>> {
        self.slot(regs, Self::ARGS).as_object().map_or_else(Vec::new, |o| o.elements().to_vec())
    }
}

/// All the state of one execution (attached to the roots between slices): the register file, and the frame stack
/// (V8 keeps its frames on the machine stack): the running frame is on top, a call pushes the callee's record in place
/// and a return pops it, nothing is copied in or out.
#[derive(Collect)]
#[collect(no_drop)]
struct State<'gc> {
    regs: Vec<Value<'gc>>,
    frames: Vec<Frame>,
    acc: Value<'gc>,
    /// V8's pending message (`SetPendingMessage` swaps it with acc)
    message: Value<'gc>,
}

impl<'gc> State<'gc> {
    /// Entering frame `f` (its part of `regs` already set up)
    /// Entering frame `f` (its part of `regs` already set up), on frame stack `frames` (empty)
    fn new(regs: Vec<Value<'gc>>, mut frames: Vec<Frame>, f: Frame) -> Self {
        frames.push(f);
        Self { regs, frames, acc: Value::Undefined, message: Value::Undefined }
    }

    /// The running frame
    #[inline]
    fn f(&self) -> &Frame {
        self.frames.last().expect("a running frame")
    }

    #[inline]
    fn f_mut(&mut self) -> &mut Frame {
        self.frames.last_mut().expect("a running frame")
    }

    /// The running frame returns: its record is popped and its part of the register file dropped; `true` if a caller
    /// goes on
    #[inline]
    fn pop_frame(&mut self) -> bool {
        let Some(done) = self.frames.pop() else { return false };
        if self.frames.is_empty() {
            return false;
        }
        self.regs.truncate(done.params);
        true
    }
}

/// How an execution stops: returning, or pausing when the instruction budget runs out.
enum Outcome<'gc> {
    Return(Value<'gc>),
    /// Instruction budget exhausted: the state is handed back as is, and the next slice continues
    Paused(Box<State<'gc>>),
}

impl<'gc> Outcome<'gc> {
    fn returned(self) -> Value<'gc> {
        match self {
            Outcome::Return(v) => v,
            Outcome::Paused(_) => unreachable!("only the outermost execution pauses"),
        }
    }
}

/// A call's arguments: values at hand, or consecutive registers of the caller's frame (copied straight into the new
/// frame).
#[derive(Clone, Copy)]
enum Args<'a, 'gc> {
    Values(&'a [Value<'gc>]),
    Regs { first: usize, count: usize },
}

impl<'gc> Args<'_, 'gc> {
    fn len(self) -> usize {
        match self {
            Args::Values(v) => v.len(),
            Args::Regs { count, .. } => count,
        }
    }

    fn get(self, regs: &[Value<'gc>], i: usize) -> Value<'gc> {
        match self {
            Args::Values(v) => v.get(i).copied().unwrap_or_default(),
            Args::Regs { first, count } if i < count => regs[first + i],
            Args::Regs { .. } => Value::Undefined,
        }
    }

    fn to_vec(self, regs: &[Value<'gc>]) -> Vec<Value<'gc>> {
        match self {
            Args::Values(v) => v.to_vec(),
            Args::Regs { first, count } => regs[first..first + count].to_vec(),
        }
    }
}

/// What to do when calling an object (taken out of the object first, then done after releasing the borrow).
enum CallStep<'gc> {
    Closure(Closure<'gc>),
    Resolver,
    Proxy,
    NativeClosure(j8framework::ClosureFn),
    Native(j8framework::Native),
    /// `call` / `apply`: if the target is callable, expands into a call to it
    Forward(j8framework::Native),
    Bound(Value<'gc>, Value<'gc>, Vec<Value<'gc>>),
}

/// Result of call resolution: a closure to enter a frame for, or the value a host function already computed.
enum Callee<'gc> {
    Closure { closure: Closure<'gc>, callee: Value<'gc>, this: Value<'gc>, args: Vec<Value<'gc>> },
    Done(Value<'gc>),
}

/// Result of preparing a construct call: a frame to enter, or the object a host constructor already produced.
enum Prepared<'gc> {
    Frame(Frame),
    Done(Value<'gc>),
}

/// A tier faster than interpretation (the JIT). Not wired to V8 bytecode yet: the executor keeps the hook so
/// embedders can still pass one.
pub trait Tier {
    /// Function `id` is about to enter its frame
    fn enter(&self, a: &Agent<'_, '_>, id: FuncId, func: &SharedFunction, regs: &mut [Value<'_>], acc: &mut Value<'_>) -> Tiered;
}

/// The JIT tier's answer.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Tiered {
    Ran,
    NotYet,
    Never,
}

/// The instruction budget of one slice, and the termination request.
struct Budget<'s> {
    tick: usize,
    left: Option<usize>,
    stop: &'s j8framework::Termination,
    terminated: bool,
}

impl<'s> Budget<'s> {
    fn new(stop: &'s j8framework::Termination, left: Option<usize>) -> Self {
        Self { tick: 0, left, stop, terminated: false }
    }

    fn remaining(&self) -> Option<usize> {
        self.left.map(|l| l + self.tick)
    }

    /// Takes the next run of steps; `true` when exhausted or terminated
    fn refill(&mut self) -> bool {
        if self.stop.observe() {
            self.terminated = true;
            return true;
        }
        let take = match &mut self.left {
            Some(0) => return true,
            Some(l) => {
                let t = (*l).min(CHECK_STEPS);
                *l -= t;
                t
            }
            None => CHECK_STEPS,
        };
        self.tick = take - 1;
        false
    }

    #[inline]
    fn spent(&mut self) -> bool {
        if self.tick == 0 {
            return self.refill();
        }
        self.tick -= 1;
        false
    }
}

/// Bytecode executor.
#[derive(Default)]
pub struct Executor {
    tier: Option<Box<dyn Tier + Send>>,
    /// Loaded functions, by function id: append-only (`eval` / the `Function` constructor also add some mid-execution),
    /// so a `&Loaded` stays valid while more are loaded; frames refer to them by id
    code: SegVec<Loaded>,
    /// Current call depth (summed over all re-entrant levels of `execute`)
    depth: Cell<usize>,
    /// Module code: first function id ⇒ module id
    module_of: RefCell<alloc::collections::BTreeMap<u32, u32>>,
    /// Bytecode shared with other executors
    cache: Option<Arc<CodeCache>>,
    /// The shapes of the contexts the loaded code makes, by [`ShapeId`] (append-only like `code`)
    context_shapes: SegVec<ContextShape>,
    /// The shape of `with` contexts, once made
    with_shape: Cell<Option<ShapeId>>,
    /// Frame stacks of finished executions, kept for the next ones (a host function calling back into JS starts one
    /// each time)
    spare_frames: RefCell<Vec<Vec<Frame>>>,
}

// An executor moves between the threads of the event loop's pool together with its isolate (one thread at a time:
// `Send`, not `Sync`)
const _: () = {
    const fn send<T: Send>() {}
    send::<Executor>();
};

/// What an instruction asks of the run loop besides going on to the next one
enum Flow<'gc> {
    Next,
    /// A new frame was pushed / the frame changed: re-fetch the function
    Frame,
    Done(Outcome<'gc>),
}

impl Executor {
    /// Adds a JIT tier (kept for embedders; not used with V8 bytecode yet).
    #[must_use]
    pub fn with_tier(mut self, tier: impl Tier + Send + 'static) -> Self {
        self.tier = Some(Box::new(tier));
        self
    }

    /// Shares compiled code through `cache` (see [`CodeCache`])
    #[must_use]
    pub fn with_cache(mut self, cache: Arc<CodeCache>) -> Self {
        self.cache = Some(cache);
        self
    }

    /// Loads a module, returning the function id of its top level.
    pub fn load(&self, module: Module) -> FuncId {
        let base = u32::try_from(self.code.len()).expect("fewer than 2^32 functions");
        for func in module.functions {
            self.code.push(Loaded::new(base, func, &self.context_shapes));
        }
        FuncId(base + Module::MAIN)
    }

    fn context_shape(&self, id: ShapeId) -> &ContextShape {
        self.context_shapes.get(id.0 as usize).expect("shape ids come from load")
    }

    #[inline]
    fn function(&self, f: FuncId) -> &Loaded {
        self.code.get(f.0 as usize).expect("function ids come from load")
    }

    // ---- frames ----

    /// Opens a frame for `closure` at the end of the register file: receiver, formal parameters (missing ones
    /// `undefined`), then the registers (all `undefined`). Sloppy non-arrow functions get the global object / a wrapper
    /// for a primitive receiver.
    fn open<'gc>(&self, a: &mut Agent<'gc, '_>, regs: &mut Vec<Value<'gc>>, closure: Closure<'gc>, callee: Value<'gc>, this: Value<'gc>, args: &[Value<'gc>]) -> Frame {
        self.open_with(a, regs, self.function(closure.func), closure, callee, this, Args::Values(args))
    }

    #[expect(clippy::too_many_arguments, reason = "the parts of a call")]
    fn open_with<'gc>(&self, a: &mut Agent<'gc, '_>, regs: &mut Vec<Value<'gc>>, loaded: &Loaded, closure: Closure<'gc>, callee: Value<'gc>, this: Value<'gc>, args: Args<'_, 'gc>) -> Frame {
        let f = &loaded.func;
        a.profiler.enter(closure.func);
        let this = match this.kind() {
            _ if !loaded.wraps_this => this,
            ValueKind::Undefined | ValueKind::Null => Value::Object(a.realm.global),
            ValueKind::Object(_) | ValueKind::Uninit => this,
            prim => a.to_object(Value::from(prim)).map_or(Value::from(prim), Value::Object),
        };
        let kept = if loaded.keeps_args { Value::Object(a.array(args.to_vec(regs))) } else { Value::Undefined };
        let params = regs.len();
        let formals = usize::from(f.bytecode.parameter_count).saturating_sub(1);
        regs.push(this);
        for i in 0..formals {
            let v = args.get(regs, i);
            regs.push(v);
        }
        // The header, from ARGS up to CONTEXT
        regs.extend_from_slice(&[kept, Value::Uninit, Value::Undefined, callee, closure.env.map_or(Value::Undefined, Value::context)]);
        let base = regs.len();
        // The register V8 puts new.target / the generator object in on entry starts `undefined` with the rest
        regs.resize(base + f.bytecode.register_count as usize, Value::Undefined);
        Frame { func: closure.func, pc: 0, insn: 0, params, base, argc: args.len() }
    }

    /// Sets the new.target register of a construct frame
    fn set_new_target<'gc>(&self, regs: &mut [Value<'gc>], f: &mut Frame, nt: Value<'gc>) {
        f.set_slot(regs, Frame::NEW_TARGET, nt);
        if let Some(r) = self.function(f.func).func.bytecode.new_target_or_generator {
            regs[f.base + usize::try_from(r).unwrap_or(0)] = nt;
        }
    }

    /// An array index from a number key
    #[inline]
    fn index(key: Value<'_>) -> Option<usize> {
        let x = key.as_number()?;
        #[expect(clippy::cast_possible_truncation, clippy::cast_sign_loss, reason = "checked by converting back")]
        let i = x as u32;
        (f64::from(i) == x && i != u32::MAX).then_some(i as usize)
    }

    /// The register file index of a register operand (locals and parameters: see [`Loaded::register`])
    #[inline]
    fn slot_of(f: &Frame, k: OperandValue) -> Option<usize> {
        (k > Loaded::SPECIAL_LIMIT).then(|| f.base.wrapping_add_signed(k as isize))
    }

    #[inline]
    fn reg<'gc>(regs: &[Value<'gc>], f: &Frame, k: OperandValue) -> Value<'gc> {
        if k > Loaded::SPECIAL_LIMIT {
            return regs[f.base.wrapping_add_signed(k as isize)];
        }
        match i32::try_from(k - Loaded::SPECIAL).unwrap_or(0) {
            Register::ARGC_FROM_FP => Value::Number(f64::from(u32::try_from(f.argc).unwrap_or(u32::MAX))),
            _ => Value::Undefined,
        }
    }

    #[inline]
    fn set_reg<'gc>(regs: &mut [Value<'gc>], f: &mut Frame, k: OperandValue, v: Value<'gc>) {
        if k > Loaded::SPECIAL_LIMIT {
            regs[f.base.wrapping_add_signed(k as isize)] = v;
        }
    }

    /// Consecutive registers from `first`
    fn reg_list<'gc>(regs: &[Value<'gc>], f: &Frame, first: OperandValue, count: OperandValue) -> Vec<Value<'gc>> {
        (0..count).map(|i| Self::reg(regs, f, first + i)).collect()
    }

    /// The context `depth` levels out from `context`
    fn context_at<'gc>(a: &Agent<'gc, '_>, context: Value<'gc>, depth: OperandValue) -> Result<JsEnv<'gc>, Error> {
        let mut e = context.as_context().ok_or_else(|| a.throw(ErrorKind::Reference, "no context"))?;
        for _ in 0..depth {
            e = e.parent.ok_or_else(|| a.throw(ErrorKind::Reference, "no outer context"))?;
        }
        Ok(e)
    }

    /// The index in an `Env` of context slot `slot` (V8 counts the fixed slots)
    fn env_index(slot: OperandValue) -> usize {
        usize::try_from(slot).unwrap_or(0).saturating_sub(Context::MIN_SLOTS as usize)
    }

    // ---- calls ----

    /// Unwraps bound functions and calls host functions directly; the remaining JS closures are left to the caller to
    /// enter a frame.
    fn resolve<'gc>(&self, a: &mut Agent<'gc, '_>, mut callee: Value<'gc>, mut this: Value<'gc>, mut args: Vec<Value<'gc>>, depth: usize) -> Result<Callee<'gc>, Error> {
        while let ValueKind::Object(o) = callee.kind() {
            let step = if let Some(c) = o.closure() {
                CallStep::Closure(c)
            } else if let Some(n) = o.native() {
                if n.forward == j8framework::Forward::No { CallStep::Native(n) } else { CallStep::Forward(n) }
            } else {
                match &o.borrow().kind {
                    ObjectKind::Function(c) => CallStep::Closure(*c),
                    ObjectKind::Resolver { .. } => CallStep::Resolver,
                    ObjectKind::Proxy(p) if p.callable => CallStep::Proxy,
                    ObjectKind::NativeClosure(c) => CallStep::NativeClosure(c.f),
                    ObjectKind::Native(n) if n.forward != j8framework::Forward::No => CallStep::Forward(*n),
                    ObjectKind::Native(n) => CallStep::Native(*n),
                    ObjectKind::Bound(b) => CallStep::Bound(b.target, b.this, b.args.clone()),
                    _ => break,
                }
            };
            if let CallStep::Forward(n) = step
                && a.is_callable(this)
            {
                let target = this;
                let mut rest = args.into_iter();
                this = rest.next().unwrap_or_default();
                args = match n.forward {
                    j8framework::Forward::Apply => match rest.next().unwrap_or_default().kind() {
                        ValueKind::Undefined | ValueKind::Null => Vec::new(),
                        list => a.list_from_array_like(Value::from(list))?,
                    },
                    _ => rest.collect(),
                };
                callee = target;
                continue;
            }
            let saved = self.depth.replace(depth);
            let r = match step {
                CallStep::Closure(c) => {
                    self.depth.set(saved);
                    let loaded = self.function(c.func);
                    if loaded.class_constructor {
                        return Err(a.throw(ErrorKind::Type, &alloc::format!("Class constructor {} cannot be invoked without 'new'", loaded.func.name)));
                    }
                    return Ok(Callee::Closure { closure: c, callee, this, args });
                }
                CallStep::Resolver => a.run_resolver(o, args.first().copied().unwrap_or_default()),
                CallStep::Proxy => a.proxy_call(o, this, &args),
                CallStep::NativeClosure(f) => {
                    let outer = core::mem::replace(&mut a.new_target, Value::Undefined);
                    let r = f(a, o, this, &args);
                    a.new_target = outer;
                    r
                }
                CallStep::Native(n) | CallStep::Forward(n) => {
                    let outer = core::mem::replace(&mut a.new_target, Value::Undefined);
                    let r = (n.f)(a, this, &args);
                    a.new_target = outer;
                    r
                }
                CallStep::Bound(target, bound_this, mut bound_args) => {
                    self.depth.set(saved);
                    bound_args.extend(args);
                    (callee, this, args) = (target, bound_this, bound_args);
                    continue;
                }
            };
            self.depth.set(saved);
            return r.map(Callee::Done);
        }
        let what = a.describe(callee);
        Err(a.throw(ErrorKind::Type, &alloc::format!("{what} is not a function")))
    }

    /// 构造一个能 `new` 的 [`j8framework::NativeClosure`]：`this` 是原型取自 `new_target` 的新对象，`f` 返回对象就用它。
    fn construct_closure<'gc>(a: &mut Agent<'gc, '_>, o: JsObject<'gc>, f: j8framework::ClosureFn, args: &[Value<'gc>], new_target: Value<'gc>) -> Result<Value<'gc>, Error> {
        let proto = a.proto_from_ctor(new_target, a.realm.object_proto)?;
        let this: Value<'gc> = ObjectData { proto: Some(proto), ..ObjectData::default() }.alloc(a.mc).into();
        let outer = core::mem::replace(&mut a.new_target, new_target);
        let r = f(a, o, this, args);
        a.new_target = outer;
        let v = r?;
        Ok(if v.as_object().is_some() { v } else { this })
    }

    /// Prepares a construct call: unwraps bound functions; host constructors compute directly; JS constructors build
    /// `this` (not for derived classes) and run instance fields.
    fn prepare<'gc>(&self, a: &mut Agent<'gc, '_>, regs: &mut Vec<Value<'gc>>, mut callee: Value<'gc>, mut args: Vec<Value<'gc>>, mut new_target: Value<'gc>, depth: usize) -> Result<Prepared<'gc>, Error> {
        while let ValueKind::Object(o) = callee.kind() {
            let kind = match &o.borrow().kind {
                ObjectKind::Function(c) => Ok(Ok(*c)),
                ObjectKind::Native(n) => Ok(Err(*n)),
                ObjectKind::NativeClosure(c) if c.constructor => {
                    let f = c.f;
                    let saved = self.depth.replace(depth);
                    let r = Self::construct_closure(a, o, f, &args, new_target);
                    self.depth.set(saved);
                    return r.map(Prepared::Done);
                }
                ObjectKind::Bound(b) => Err(Some((b.target, b.args.clone()))),
                ObjectKind::Proxy(p) if p.constructor => Err(None),
                _ => break,
            };
            match kind {
                Ok(Ok(closure)) => {
                    let loaded = self.function(closure.func);
                    if !(loaded.constructs || loaded.class_constructor) {
                        break;
                    }
                    let this = if loaded.derived {
                        None
                    } else {
                        let this = Self::create_this(a, loaded, new_target)?;
                        if let Some(init) = closure.fields {
                            a.call(init.into(), this.into(), &[])?;
                        }
                        Some(Value::Object(this))
                    };
                    let mut f = self.open(a, regs, closure, callee, this.unwrap_or(Value::Uninit), &args);
                    self.set_new_target(regs, &mut f, new_target);
                    f.set_slot(regs, Frame::NEW_THIS, this.unwrap_or(Value::Uninit));
                    return Ok(Prepared::Frame(f));
                }
                Ok(Err(native)) => {
                    if !native.constructor {
                        break;
                    }
                    let saved = self.depth.replace(depth);
                    let outer = core::mem::replace(&mut a.new_target, new_target);
                    let r = (native.f)(a, Value::Undefined, &args);
                    a.new_target = outer;
                    self.depth.set(saved);
                    let Some(obj) = (r?).as_object() else { unreachable!("built-in constructors return objects") };
                    return Ok(Prepared::Done(obj.into()));
                }
                Err(None) => {
                    let saved = self.depth.replace(depth);
                    let r = a.proxy_construct(o, &args, new_target);
                    self.depth.set(saved);
                    return r.map(Prepared::Done);
                }
                Err(Some((target, mut bound_args))) => {
                    if matches!(new_target.kind(), ValueKind::Object(t) if Gc::ptr_eq(t, o)) {
                        new_target = target;
                    }
                    bound_args.extend(args);
                    (callee, args) = (target, bound_args);
                }
            }
        }
        let what = a.describe(callee);
        Err(a.throw(ErrorKind::Type, &alloc::format!("{what} is not a constructor")))
    }

    /// Builds `this` during construction: the prototype is `new.target.prototype`.
    fn create_this<'gc>(a: &mut Agent<'gc, '_>, ctor: &Loaded, new_target: Value<'gc>) -> Result<JsObject<'gc>, Error> {
        let proto = match new_target.kind() {
            ValueKind::Object(o) => match ctor.prototype.get(o) {
                Some(v) => v,
                None => {
                    let v = a.get_named(new_target, "prototype")?;
                    ctor.prototype.fill_get(o, "prototype");
                    v
                }
            },
            _ => a.get_named(new_target, "prototype")?,
        };
        let proto = proto.as_object().unwrap_or(a.realm.object_proto);
        Ok(ObjectData::ordinary(a.mc, Some(proto), usize::from(ctor.instance_size.get())))
    }

    /// A construct call returns: an object result is used, otherwise `this`.
    fn construct_result<'gc>(&self, a: &Agent<'gc, '_>, regs: &[Value<'gc>], f: &Frame, v: Value<'gc>) -> Result<Value<'gc>, Error> {
        let loaded = self.function(f.func);
        if v.as_object().is_some() {
            return Ok(v);
        }
        let r = match (loaded.derived, f.new_this(regs)) {
            (true, _) if v != Value::Undefined => Err(a.throw(ErrorKind::Type, "Derived constructors may only return object or undefined")),
            (true, None) => Err(a.throw(ErrorKind::Reference, "Must call super constructor in derived class before accessing 'this' or returning from derived constructor")),
            (_, Some(t)) => Ok(t),
            (_, None) => Ok(v),
        };
        if let Ok(ValueKind::Object(o)) = r.as_ref().map(|v| v.kind()) {
            loaded.instance_size.set(u8::try_from(o.borrow().props().len()).unwrap_or(u8::MAX));
        }
        r
    }

    /// Creates a function object; ordinary functions get a `prototype` (whose `constructor` points back).
    fn function_object<'gc>(&self, a: &Agent<'gc, '_>, closure: Closure<'gc>) -> JsObject<'gc> {
        let loaded = self.function(closure.func);
        let func = &loaded.func;
        let proto = match (loaded.generator, loaded.asynchronous) {
            (true, true) => a.realm.async_generator_function_proto,
            (true, false) => a.realm.generator_function_proto,
            (false, true) => a.realm.async_function_proto,
            (false, false) => a.realm.function_proto,
        };
        let fo = ObjectData { kind: ObjectKind::Function(closure), proto: Some(proto), ..ObjectData::default() }.alloc(a.mc);
        let length = Value::Number(f64::from(func.length));
        let name = Value::String(a.mc.alloc(j8framework::JsStr::from(loaded.name.clone())));
        // `length`, `name`, then `prototype` for generators and ordinary functions / classes
        let prototype = if loaded.generator {
            let base = if loaded.asynchronous { a.realm.async_generator_proto } else { a.realm.generator_proto };
            Some((ObjectData { proto: Some(base), ..ObjectData::default() }.alloc(a.mc), true))
        } else if !loaded.no_prototype && !loaded.asynchronous {
            let p = a.object();
            match &*loaded.prototype_shape.borrow() {
                Some(shape) => p.fill(a.mc, shape, [Slot::Data(fo.into())]),
                None => p.borrow_mut(a.mc).define("constructor", Property::hidden(fo.into())),
            }
            Self::remember_shape(&loaded.prototype_shape, p, 1);
            Some((p, !loaded.class_constructor))
        } else {
            None
        };
        match &*loaded.function_shape.borrow() {
            Some(shape) => fo.fill(a.mc, shape, [Slot::Data(length), Slot::Data(name)].into_iter().chain(prototype.map(|(p, _)| Slot::Data(p.into())))),
            None => {
                let prop = |v: Value<'gc>| Property { slot: Slot::Data(v), enumerable: false, writable: false, configurable: true };
                let d = fo.borrow_mut(a.mc);
                d.define("length", prop(length));
                d.define("name", prop(name));
                if let Some((p, writable)) = prototype {
                    d.define("prototype", Property { slot: Slot::Data(p.into()), enumerable: false, writable, configurable: false });
                }
            }
        }
        Self::remember_shape(&loaded.function_shape, fo, 2 + usize::from(prototype.is_some()));
        fo
    }

    /// `CreateObjectLiteral` of boilerplate constant `k`: in one step once its shape is known (as V8 copies the
    /// boilerplate with its map), property by property the first time
    fn object_literal<'gc>(a: &Agent<'gc, '_>, loaded: &Loaded, k: OperandValue) -> JsObject<'gc> {
        let k = usize::try_from(k).unwrap_or(0);
        let Some(Constant::Object(b)) = loaded.func.bytecode.constants.get(k) else { unreachable!("an object boilerplate") };
        let ob = a.object_with(b.properties.len());
        let cache = &loaded.literals[k];
        let value = |v: &Option<LiteralValue>| v.as_ref().map_or(Value::Undefined, |v| Self::literal(a, v));
        match &*cache.borrow() {
            Some(shape) => {
                ob.fill(a.mc, shape, b.properties.iter().map(|(_, v)| Slot::Data(value(v))));
                return ob;
            }
            None => {
                for (key, v) in &b.properties {
                    ob.borrow_mut(a.mc).define(key, Property::data(value(v)));
                }
            }
        }
        Self::remember_shape(cache, ob, b.properties.len());
        ob
    }

    /// After making an object the slow way: keeps its shape for next time (when it has exactly the `n` properties
    /// expected, in fast mode)
    fn remember_shape(cache: &RefCell<Option<Arc<Shape>>>, o: JsObject<'_>, n: usize) {
        if cache.borrow().is_some() {
            return;
        }
        let shape = o.get().props().shape().filter(|s| s.len() == n);
        *cache.borrow_mut() = shape;
    }

    // ---- globals and lookups by name ----

    /// The script context slot holding `name`, if a script declared it lexically
    fn script_slot<'gc>(&self, a: &Agent<'gc, '_>, name: &str) -> Option<(JsEnv<'gc>, usize, bool)> {
        a.realm.script_contexts.get().iter().find_map(|e| {
            let (i, n) = self.context_shape(e.shape?).slot(name)?;
            Some((*e, i, n.constant))
        })
    }

    #[cold]
    fn uninitialized(a: &Agent<'_, '_>, name: &str) -> Error {
        a.throw(ErrorKind::Reference, &alloc::format!("Cannot access '{name}' before initialization"))
    }

    /// `LdaGlobal`: script contexts first, then the global object (through the cache); `typeof` gives `undefined` for
    /// a name that isn't there
    fn global_load<'gc>(&self, a: &mut Agent<'gc, '_>, cache: &ic::GlobalIc, name: &str, inside_typeof: bool) -> Result<Value<'gc>, Error> {
        // A script's `this`, read from a function inside it (V8 keeps it in the script context table)
        if name == SCRIPT_THIS {
            return Ok(Value::Object(a.realm.global));
        }
        // A hit means the name is still a global property: a script declaring it lexically changes the script scope's
        // shape
        let (scope, global) = (a.realm.script_scope, a.realm.global);
        if let Some(v) = cache.get(scope, global) {
            return Ok(v);
        }
        if let Some((e, i, _)) = self.script_slot(a, name) {
            let v = e.load(i);
            return if matches!(v.kind(), ValueKind::Uninit) { Err(Self::uninitialized(a, name)) } else { Ok(v) };
        }
        let g = Value::Object(global);
        if !a.has_property(g, name)? {
            if inside_typeof {
                return Ok(Value::Undefined);
            }
            return Err(a.throw(ErrorKind::Reference, &alloc::format!("{name} is not defined")));
        }
        let v = a.get_named(g, name)?;
        cache.fill(scope, global, name);
        Ok(v)
    }

    /// `StaGlobal`: a script context slot (TDZ / `const` checked), else the global object (strict: must exist)
    fn global_store<'gc>(&self, a: &mut Agent<'gc, '_>, cache: &ic::GlobalIc, name: &str, v: Value<'gc>, strict: bool) -> Result<(), Error> {
        let (scope, global) = (a.realm.script_scope, a.realm.global);
        if let Some((o, at)) = cache.writable(scope, global)
            && let Slot::Data(_) = o.get().props().slot(at)
        {
            o.get().set_at(a.mc, at, v);
            return Ok(());
        }
        if let Some((e, i, constant)) = self.script_slot(a, name) {
            if matches!(e.load(i).kind(), ValueKind::Uninit) {
                return Err(Self::uninitialized(a, name));
            }
            if constant {
                return Err(a.throw(ErrorKind::Type, "Assignment to constant variable."));
            }
            Env::set(e, a.mc, i, v);
            return Ok(());
        }
        let g = Value::Object(global);
        if strict && !a.has_property(g, name)? {
            return Err(a.throw(ErrorKind::Reference, &alloc::format!("{name} is not defined")));
        }
        a.put_named(g, name, v, strict)?;
        cache.fill(scope, global, name);
        Ok(())
    }

    /// A lookup by name through the context chain from `env` (V8's `LoadLookupSlot`): `with` objects, sloppy eval
    /// extension objects and named slots, then the script contexts and the global object. Gives the holder found:
    /// a context slot, an object (with its `this` for a call), or nothing.
    fn lookup<'gc>(&self, a: &mut Agent<'gc, '_>, env: Option<JsEnv<'gc>>, name: &str) -> Result<Found<'gc>, Error> {
        let mut e = env;
        while let Some(c) = e {
            if let Some(ext) = c.extension {
                let with = c.shape.is_some_and(|s| self.context_shape(s).kind == ContextKind::With);
                let has = if with { a.has_property(Value::Object(ext), name)? } else { ext.borrow().get_own(name).is_some() };
                if has && !(with && Self::unscopable(a, ext, name)?) {
                    return Ok(Found::Object(ext));
                }
            }
            if let Some((i, n)) = c.shape.and_then(|s| self.context_shape(s).slot(name)) {
                return Ok(Found::Slot(c, i, n.constant));
            }
            e = c.parent;
        }
        if let Some((e, i, constant)) = self.script_slot(a, name) {
            return Ok(Found::Slot(e, i, constant));
        }
        let g = Value::Object(a.realm.global);
        if a.has_property(g, name)? {
            return Ok(Found::Object(a.realm.global));
        }
        Ok(Found::Nothing)
    }

    /// `name` is blocked by the `with` object's `@@unscopables`
    fn unscopable<'gc>(a: &mut Agent<'gc, '_>, o: JsObject<'gc>, name: &str) -> Result<bool, Error> {
        let u = a.get_symbol(Value::Object(o), a.realm.symbols.unscopables, Value::Object(o))?;
        if u.as_object().is_none() {
            return Ok(false);
        }
        let blocked = a.get_named(u, name)?;
        Ok(a.to_boolean(blocked))
    }

    fn lookup_load<'gc>(&self, a: &mut Agent<'gc, '_>, env: Option<JsEnv<'gc>>, name: &str, inside_typeof: bool) -> Result<Value<'gc>, Error> {
        match self.lookup(a, env, name)? {
            Found::Slot(c, i, _) => {
                let v = c.load(i);
                if matches!(v.kind(), ValueKind::Uninit) { Err(Self::uninitialized(a, name)) } else { Ok(v) }
            }
            Found::Object(o) => a.get_named(Value::Object(o), name),
            // `this` of direct eval code at the top level of a script
            Found::Nothing if name == "this" => Ok(Value::Object(a.realm.global)),
            Found::Nothing if inside_typeof => Ok(Value::Undefined),
            Found::Nothing => Err(a.throw(ErrorKind::Reference, &alloc::format!("{name} is not defined"))),
        }
    }

    fn lookup_store<'gc>(&self, a: &mut Agent<'gc, '_>, env: Option<JsEnv<'gc>>, name: &str, v: Value<'gc>, strict: bool) -> Result<(), Error> {
        match self.lookup(a, env, name)? {
            Found::Slot(c, i, constant) => {
                if matches!(c.load(i).kind(), ValueKind::Uninit) {
                    return Err(Self::uninitialized(a, name));
                }
                if constant {
                    return Err(a.throw(ErrorKind::Type, "Assignment to constant variable."));
                }
                Env::set(c, a.mc, i, v);
                Ok(())
            }
            Found::Object(o) => a.put_named(Value::Object(o), name, v, strict),
            Found::Nothing if strict => Err(a.throw(ErrorKind::Reference, &alloc::format!("{name} is not defined"))),
            Found::Nothing => a.put_named(Value::Object(a.realm.global), name, v, false),
        }
    }

    /// The variable environment for eval declarations from `env`: the nearest function context with an extension
    /// object, or the global object
    fn var_scope<'gc>(&self, a: &Agent<'gc, '_>, env: Option<JsEnv<'gc>>) -> JsObject<'gc> {
        let mut e = env;
        while let Some(c) = e {
            if let (Some(ext), Some(ContextKind::Function)) = (c.extension, c.shape.map(|s| self.context_shape(s).kind)) {
                return ext;
            }
            e = c.parent;
        }
        a.realm.global
    }

    /// `DeclareEvalVar` / `DeclareEvalFunction`: a lexical declaration with the same name between here and the variable
    /// environment is a SyntaxError
    fn declare_eval<'gc>(&self, a: &mut Agent<'gc, '_>, env: Option<JsEnv<'gc>>, name: &str, value: Option<Value<'gc>>) -> Result<(), Error> {
        let mut e = env;
        while let Some(c) = e {
            if let Some(s) = c.shape.map(|s| self.context_shape(s)) {
                if s.kind == ContextKind::Function && c.extension.is_some() {
                    break;
                }
                if s.slot(name).is_some_and(|(_, n)| n.lexical) {
                    return Err(a.throw(ErrorKind::Syntax, &alloc::format!("Identifier '{name}' has already been declared")));
                }
            }
            e = c.parent;
        }
        let target = self.var_scope(a, env);
        if Gc::ptr_eq(target, a.realm.global) && self.script_slot(a, name).is_some() {
            return Err(a.throw(ErrorKind::Syntax, &alloc::format!("Identifier '{name}' has already been declared")));
        }
        let existing = target.borrow().get_own(name);
        match (existing, value) {
            (Some(_), None) => {}
            (Some(p), Some(v)) if !p.configurable => a.put_named(Value::Object(target), name, v, false)?,
            (_, v) => target.borrow_mut(a.mc).define(name, Property { slot: Slot::Data(v.unwrap_or_default()), enumerable: true, writable: true, configurable: true }),
        }
        Ok(())
    }

    /// `DeclareGlobals`: a script's `var`s and functions on the global object, after checking all can be declared
    fn declare_globals<'gc>(&self, a: &mut Agent<'gc, '_>, f: &Frame, env: Option<JsEnv<'gc>>, decls: &[j8bytecode::GlobalDeclaration]) -> Result<(), Error> {
        for d in decls {
            let name = &*d.name;
            if self.script_slot(a, name).is_some() {
                return Err(a.throw(ErrorKind::Syntax, &alloc::format!("Identifier '{name}' has already been declared")));
            }
            let (existing, extensible) = {
                let g = a.realm.global.borrow();
                (g.get_own(name), !g.non_extensible())
            };
            match existing {
                Some(p) if d.function.is_some() && !p.configurable && !(p.writable && p.enumerable && matches!(p.slot, Slot::Data(_))) => return Err(a.throw(ErrorKind::Type, &alloc::format!("Cannot redefine property: {name}"))),
                None if !extensible => return Err(a.throw(ErrorKind::Type, &alloc::format!("Cannot declare global {name}: the global object is not extensible"))),
                _ => {}
            }
        }
        let base = self.function(f.func).base;
        for d in decls {
            let name = &*d.name;
            let global = a.realm.global;
            let existing = global.borrow().get_own(name);
            let value = d.function.map(|n| {
                let closure = Closure::new(FuncId(base + n), env);
                Value::Object(self.function_object(a, closure))
            });
            let fresh = Property { slot: Slot::Data(value.unwrap_or_default()), enumerable: true, writable: true, configurable: false };
            let prop = match (existing, value) {
                (None, _) => fresh,
                (Some(_), None) => continue,
                (Some(p), Some(_)) if p.configurable => fresh,
                (Some(p), Some(v)) => Property { slot: Slot::Data(v), ..p },
            };
            global.borrow_mut(a.mc).define(name, prop);
        }
        Ok(())
    }

    /// Creates the script context of script `main` (its top-level lexical declarations, holes until initialized) and
    /// registers it in the realm's script context table, after checking nothing else declares those names
    fn script_context<'gc>(&self, a: &mut Agent<'gc, '_>, main: FuncId) -> Result<Option<JsEnv<'gc>>, Error> {
        let Some(id) = self.function(main).scope else { return Ok(None) };
        let shape = self.context_shape(id);
        if shape.kind != ContextKind::Script {
            return Ok(None);
        }
        for n in shape.names.iter() {
            let name = &*n.name;
            let restricted = a.realm.global.borrow().get_own(name).is_some_and(|p| !p.configurable);
            if self.script_slot(a, name).is_some() || restricted {
                return Err(a.throw(ErrorKind::Syntax, &alloc::format!("Identifier '{name}' has already been declared")));
            }
        }
        let env = Env::context(shape.names.len(), Value::Uninit, None, Some(id), None).alloc(a.mc);
        a.realm.script_contexts.update(a.mc, |v| {
            let mut v = v.clone();
            v.push(env);
            v
        });
        // Global name caches key on the script scope's shape: a new lexical name may shadow a global property
        for n in shape.names.iter() {
            a.realm.script_scope.borrow_mut(a.mc).define(&n.name, Property::data(Value::Undefined));
        }
        Ok(Some(env))
    }

    // ---- running ----

    /// Initial execution state for script `main`: its script context made, `this` the global object.
    fn script_state<'gc>(&self, a: &mut Agent<'gc, '_>, main: FuncId) -> Result<State<'gc>, Error> {
        let env = self.script_context(a, main)?;
        let this = Value::Object(a.realm.global);
        let mut regs = Vec::new();
        let f = self.open(a, &mut regs, Closure::new(main, env), Value::Undefined, this, &[]);
        Ok(self.state(regs, f))
    }

    /// Runs script `main` (`this` is the global object).
    pub fn run_script<'gc>(&self, a: &mut Agent<'gc, '_>, main: FuncId) -> Result<Value<'gc>, Error> {
        let st = self.script_state(a, main)?;
        self.execute(a, st, &mut None).map(Outcome::returned)
    }

    /// Finds a handler for exception `v` in the frames, innermost first (the handler table entry whose range holds the
    /// throwing instruction, the innermost such range); `false` once all frames are popped.
    fn unwind<'gc>(&self, st: &mut State<'gc>, v: Value<'gc>) -> bool {
        loop {
            let loaded = self.function(st.f().func);
            let at = loaded.insns.get(st.f().insn).map_or(u32::MAX, |d| d.start);
            let h = loaded.func.bytecode.handlers.iter().filter(|h| h.start <= at && at < h.end).min_by_key(|h| h.end - h.start).copied();
            if let Some(h) = h {
                let ctx = Self::reg(&st.regs, st.f(), OperandValue::from(h.context));
                let f = *st.f();
                f.set_env(&mut st.regs, ctx.as_context());
                st.f_mut().pc = loaded.index_of(h.handler as usize);
                st.acc = v;
                return true;
            }
            if !st.pop_frame() {
                return false;
            }
        }
    }

    /// Runs until the outermost frame returns; an exception first looks for a handler and propagates outward only if
    /// not caught. `budget` applies only to the outermost level: when it runs out, stops with the state.
    fn execute<'gc>(&self, a: &mut Agent<'gc, '_>, mut st: State<'gc>, budget: &mut Option<usize>) -> Result<Outcome<'gc>, Error> {
        let base = self.depth.get();
        let mut b = Budget::new(a.stop, *budget);
        let r = loop {
            match self.run(a, &mut st, base, &mut b) {
                Ok(Some(v)) => break Ok(Some(v)),
                Ok(None) if b.terminated => break Err(Error::Terminated),
                Ok(None) => break Ok(None),
                Err(Error::Exception(g)) => {
                    let v = a.local(&g);
                    if !self.unwind(&mut st, v) {
                        break Err(Error::Exception(g));
                    }
                }
                Err(e) => break Err(e),
            }
        };
        let r = match r {
            // Paused: the state carries on in the next slice
            Ok(None) => Ok(Outcome::Paused(Box::new(st))),
            r => {
                self.recycle(a, st);
                r.map(|v| v.expect("an outcome"))
            }
        };
        *budget = b.remaining();
        self.depth.set(base);
        r
    }

    /// Main loop: runs each frame's threaded code until the frame changes. `None` when the budget runs out (the state
    /// is intact).
    fn run<'gc>(&self, a: &mut Agent<'gc, '_>, st: &mut State<'gc>, depth: usize, budget: &mut Budget<'_>) -> Result<Option<Outcome<'gc>>, Error> {
        loop {
            if budget.spent() {
                return Ok(None);
            }
            let loaded = self.function(st.f().func);
            let mut cx = threaded::Cx { exe: self, a, st, loaded, budget, depth };
            match Self::threaded(&mut cx) {
                threaded::Exit::Frame => {}
                threaded::Exit::Done(o) => return Ok(Some(o)),
                threaded::Exit::Throw(e) => return Err(e),
                #[cfg(not(tail_calls))]
                threaded::Exit::Next(..) => unreachable!("the loop in `threaded` takes these"),
            }
        }
    }

    /// Enters a frame for a call from the current one (the caller resumes at its next instruction)
    #[inline]
    fn push_frame(st: &mut State<'_>, next: Frame) {
        st.frames.push(next);
    }

    /// A call from bytecode: a JS closure gets a frame, host functions run here
    #[expect(clippy::too_many_arguments, reason = "the parts of a call")]
    fn call_from<'gc>(&self, a: &mut Agent<'gc, '_>, st: &mut State<'gc>, callee: Value<'gc>, this: Value<'gc>, args: Args<'_, 'gc>, level: usize) -> Result<Flow<'gc>, Error> {
        if level >= STACK_LIMIT {
            return Err(a.throw(ErrorKind::Range, "Maximum call stack size exceeded"));
        }
        // An ordinary JS function: a frame straight from the arguments
        if let ValueKind::Object(o) = callee.kind()
            && let Some(c) = o.closure()
        {
            let l = self.function(c.func);
            if !(l.class_constructor || l.generator || l.asynchronous) {
                let next = self.open_with(a, &mut st.regs, l, c, callee, this, args);
                Self::push_frame(st, next);
                return Ok(Flow::Frame);
            }
        }
        let args = args.to_vec(&st.regs);
        match self.resolve(a, callee, this, args, level)? {
            Callee::Closure { closure, callee, this, args } => {
                let next = self.open(a, &mut st.regs, closure, callee, this, &args);
                Self::push_frame(st, next);
                Ok(Flow::Frame)
            }
            Callee::Done(v) => {
                st.acc = v;
                Ok(Flow::Next)
            }
        }
    }

    fn construct_from<'gc>(&self, a: &mut Agent<'gc, '_>, st: &mut State<'gc>, callee: Value<'gc>, args: Vec<Value<'gc>>, new_target: Value<'gc>, level: usize) -> Result<Flow<'gc>, Error> {
        if level >= STACK_LIMIT {
            return Err(a.throw(ErrorKind::Range, "Maximum call stack size exceeded"));
        }
        match self.prepare(a, &mut st.regs, callee, args, new_target, level)? {
            Prepared::Frame(next) => {
                Self::push_frame(st, next);
                Ok(Flow::Frame)
            }
            Prepared::Done(v) => {
                st.acc = v;
                Ok(Flow::Next)
            }
        }
    }

    /// The value a constant pool entry loads as (`LdaConstant`)
    fn constant<'gc>(a: &Agent<'gc, '_>, c: &Constant) -> Value<'gc> {
        match c {
            Constant::Number(x) => Value::Number(*x),
            Constant::String(s) => a.string(s).into(),
            Constant::BigInt(digits) => a.bigint(Agent::parse_bigint(digits).unwrap_or_default()),
            Constant::AsyncIteratorSymbol => a.realm.symbols.async_iterator.into(),
            _ => Value::Undefined,
        }
    }

    fn literal<'gc>(a: &Agent<'gc, '_>, v: &LiteralValue) -> Value<'gc> {
        match v {
            LiteralValue::Number(x) => Value::Number(*x),
            LiteralValue::String(s) => a.string(s).into(),
            LiteralValue::Bool(b) => Value::Bool(*b),
            LiteralValue::Null => Value::Null,
            LiteralValue::Undefined | LiteralValue::Hole => Value::Undefined,
        }
    }

    /// A string constant's text
    fn name_at(loaded: &Loaded, k: OperandValue) -> &str {
        match loaded.func.bytecode.constants.get(usize::try_from(k).unwrap_or(usize::MAX)) {
            Some(Constant::String(s)) => s,
            _ => "",
        }
    }

    #[expect(clippy::too_many_lines, reason = "one arm per bytecode")]
    fn step<'gc>(&self, a: &mut Agent<'gc, '_>, st: &mut State<'gc>, loaded: &Loaded, d: &Insn, level: usize, budget: &mut Budget<'_>) -> Result<Flow<'gc>, Error> {
        let o = &d.operands;
        let State { regs, frames, acc, message } = st;
        let f = frames.last_mut().expect("a running frame");
        macro_rules! r {
            ($i:expr) => {
                Self::reg(regs, f, o[$i])
            };
        }
        let strict = loaded.func.strict;
        // Two numbers inline; anything else through the agent
        macro_rules! arith {
            ($l:expr, $r:expr, $op:ident, $sym:tt) => {{
                let (l, r) = ($l, $r);
                *acc = match (l.as_number(), r.as_number()) {
                    (Some(x), Some(y)) => Value::Number(x $sym y),
                    _ => a.binary(BinaryOp::$op, l, r)?,
                };
            }};
        }
        macro_rules! relational {
            ($op:ident, $sym:tt) => {{
                let l = r!(0);
                *acc = match (l.as_number(), acc.as_number()) {
                    (Some(x), Some(y)) => Value::Bool(x $sym y),
                    _ => Value::Bool(a.compare(CompareOp::$op, l, *acc)?),
                };
            }};
        }
        let binary = |op: BinaryOp, a: &mut Agent<'gc, '_>, l: Value<'gc>, r: Value<'gc>| -> Result<Value<'gc>, Error> {
            match (l.as_number(), r.as_number()) {
                (Some(x), Some(y)) => Ok(op.numbers(x, y).into()),
                _ => a.binary(op, l, r),
            }
        };
        let compare = |op: CompareOp, a: &mut Agent<'gc, '_>, l: Value<'gc>, r: Value<'gc>| -> Result<Value<'gc>, Error> {
            match (l.as_number(), r.as_number()) {
                (Some(x), Some(y)) => Ok(op.numbers(x, y).into()),
                _ => a.compare(op, l, r).map(Value::Bool),
            }
        };
        use Bytecode as B;
        match d.bytecode {
            B::LdaZero => *acc = Value::Number(0.0),
            B::LdaSmi => *acc = Value::Number(o[0] as f64),
            B::LdaUndefined => *acc = Value::Undefined,
            B::LdaNull => *acc = Value::Null,
            B::LdaTheHole => *acc = Value::Uninit,
            B::LdaTrue => *acc = Value::Bool(true),
            B::LdaFalse => *acc = Value::Bool(false),
            B::LdaConstant => {
                let k = usize::try_from(o[0]).unwrap_or(0);
                *acc = match &loaded.func.bytecode.constants[k] {
                    // A class boilerplate goes to `DefineClass` as its index in the constant pool
                    Constant::Class(_) => Value::Number(k as f64),
                    c => Self::constant(a, c),
                };
            }
            B::Ldar => *acc = r!(0),
            B::Star => Self::set_reg(regs, f, o[0], *acc),
            B::Mov => {
                let v = r!(0);
                Self::set_reg(regs, f, o[1], v);
            }
            B::LdaGlobal | B::LdaGlobalInsideTypeof => {
                let name = Self::name_at(loaded, o[0]);
                *acc = self.global_load(a, &loaded.globals[usize::try_from(o[1]).unwrap_or(0)], name, d.bytecode == B::LdaGlobalInsideTypeof)?;
            }
            B::StaGlobal => {
                let name = Self::name_at(loaded, o[0]);
                let slot = usize::try_from(o[1]).unwrap_or(0);
                let strict_slot = loaded.func.bytecode.feedback.get(slot) == Some(&j8bytecode::FeedbackSlotKind::StoreGlobalStrict);
                self.global_store(a, &loaded.globals[slot], name, *acc, strict_slot || strict)?;
            }
            B::LdaCurrentContextSlot | B::LdaCurrentContextSlotNoCell | B::LdaImmutableCurrentContextSlot => {
                let e = f.env(regs).ok_or_else(|| a.throw(ErrorKind::Reference, "no context"))?;
                *acc = e.load(Self::env_index(o[0]));
            }
            B::LdaContextSlot | B::LdaContextSlotNoCell | B::LdaImmutableContextSlot => {
                let e = Self::context_at(a, r!(0), o[2])?;
                *acc = e.load(Self::env_index(o[1]));
            }
            B::StaCurrentContextSlot | B::StaCurrentContextSlotNoCell => {
                let e = f.env(regs).ok_or_else(|| a.throw(ErrorKind::Reference, "no context"))?;
                Env::set(e, a.mc, Self::env_index(o[0]), *acc);
            }
            B::StaContextSlot | B::StaContextSlotNoCell => {
                let e = Self::context_at(a, r!(0), o[2])?;
                Env::set(e, a.mc, Self::env_index(o[1]), *acc);
            }
            B::PushContext => {
                let cur = f.env(regs).map_or(Value::Undefined, Value::context);
                Self::set_reg(regs, f, o[0], cur);
                f.set_env(regs, acc.as_context());
            }
            B::PopContext => {
                let e = r!(0).as_context();
                f.set_env(regs, e);
            }
            B::CreateFunctionContext | B::CreateEvalContext => {
                let shape = loaded.shapes[usize::try_from(o[0]).unwrap_or(0)];
                let n = usize::try_from(o[1]).unwrap_or(0);
                let extension = loaded.extension.then(|| ObjectData::default().alloc(a.mc));
                *acc = Value::context(Env::context(n, Value::Undefined, f.env(regs), shape, extension).alloc(a.mc));
            }
            B::CreateBlockContext => {
                let shape = loaded.shapes[usize::try_from(o[0]).unwrap_or(0)];
                let n = shape.map_or(0, |s| self.context_shape(s).names.len());
                *acc = Value::context(Env::context(n, Value::Undefined, f.env(regs), shape, None).alloc(a.mc));
            }
            B::CreateCatchContext => {
                let shape = loaded.shapes[usize::try_from(o[1]).unwrap_or(0)];
                let e = Env::context(1, r!(0), f.env(regs), shape, None).alloc(a.mc);
                *acc = Value::context(e);
            }
            B::CreateWithContext => {
                let obj = r!(0).as_object();
                let shape = Some(self.with_shape.get().unwrap_or_else(|| {
                    let id = ShapeId(u32::try_from(self.context_shapes.push(ContextShape { kind: ContextKind::With, names: Box::new([]) })).expect("fewer than 2^32 context shapes"));
                    self.with_shape.set(Some(id));
                    id
                }));
                *acc = Value::context(Env::context(0, Value::Undefined, f.env(regs), shape, obj).alloc(a.mc));
            }
            B::LdaLookupSlot | B::LdaLookupSlotInsideTypeof | B::LdaLookupGlobalSlot | B::LdaLookupGlobalSlotInsideTypeof | B::LdaLookupContextSlot | B::LdaLookupContextSlotInsideTypeof | B::LdaLookupContextSlotNoCell | B::LdaLookupContextSlotNoCellInsideTypeof => {
                let name = Self::name_at(loaded, o[0]);
                let inside = matches!(d.bytecode, B::LdaLookupSlotInsideTypeof | B::LdaLookupGlobalSlotInsideTypeof | B::LdaLookupContextSlotInsideTypeof | B::LdaLookupContextSlotNoCellInsideTypeof);
                *acc = self.lookup_load(a, f.env(regs), name, inside)?;
            }
            B::StaLookupSlot => {
                let name = Self::name_at(loaded, o[0]);
                let s = u8::try_from(o[1]).unwrap_or(0) & j8bytecode::StoreLookupSlotFlags::STRICT != 0;
                self.lookup_store(a, f.env(regs), name, *acc, s)?;
            }
            B::GetNamedProperty => {
                let obj = r!(0);
                let name = Self::name_at(loaded, o[1]);
                let ic = &loaded.ics[usize::try_from(o[2]).unwrap_or(0)];
                *acc = match obj.kind() {
                    ValueKind::Object(ob) => match ic.get(ob) {
                        Some(v) => v,
                        None => {
                            let v = a.get_named(obj, name)?;
                            ic.fill_get(ob, name);
                            v
                        }
                    },
                    _ => a.get_named(obj, name)?,
                };
            }
            B::SetNamedProperty => {
                let obj = r!(0);
                let name = Self::name_at(loaded, o[1]);
                let slot = usize::try_from(o[2]).unwrap_or(0);
                let s = loaded.func.bytecode.feedback.get(slot) == Some(&j8bytecode::FeedbackSlotKind::SetNamedStrict);
                let ic = &loaded.ics[slot];
                match obj.kind() {
                    ValueKind::Object(ob) => {
                        if !ic.set(a.mc, ob, *acc, true) {
                            let before = ic::Before::from(ob);
                            a.put_named(obj, name, *acc, s)?;
                            ic.fill_set(ob, name, before, true);
                        }
                    }
                    _ => a.put_named(obj, name, *acc, s)?,
                }
            }
            B::DefineNamedOwnProperty => {
                let name = Self::name_at(loaded, o[1]);
                if let Some(ob) = r!(0).as_object() {
                    let ic = &loaded.ics[usize::try_from(o[2]).unwrap_or(0)];
                    if !ic.set(a.mc, ob, *acc, false) {
                        let before = ic::Before::from(ob);
                        a.define(ob, name, Property::data(*acc))?;
                        ic.fill_set(ob, name, before, false);
                    }
                }
            }
            B::GetKeyedProperty => *acc = a.get_keyed(r!(0), *acc)?,
            B::SetKeyedProperty => {
                let slot = usize::try_from(o[2]).unwrap_or(0);
                let s = loaded.func.bytecode.feedback.get(slot) == Some(&j8bytecode::FeedbackSlotKind::SetKeyedStrict);
                a.put_keyed(r!(0), r!(1), *acc, s)?;
            }
            B::DefineKeyedOwnProperty | B::DefineKeyedOwnPropertyInLiteral | B::StaInArrayLiteral => {
                // V8's `kSetFunctionName`: an anonymous function value is named after the (computed) key
                if d.bytecode != B::StaInArrayLiteral
                    && o[2] & SET_FUNCTION_NAME != 0
                    && let Some(fo) = acc.as_object()
                {
                    a.set_function_name(fo, r!(1), None)?;
                }
                if let Some(ob) = r!(0).as_object() {
                    a.define_keyed(ob, r!(1), Property::data(*acc))?;
                }
            }
            B::DeletePropertyStrict | B::DeletePropertySloppy => {
                let ok = a.delete_keyed(r!(0), *acc)?;
                if d.bytecode == B::DeletePropertyStrict && !ok {
                    return Err(a.throw(ErrorKind::Type, "Cannot delete a non-configurable property"));
                }
                *acc = Value::Bool(ok);
            }
            B::Add => arith!(r!(0), *acc, Add, +),
            B::Sub => arith!(r!(0), *acc, Sub, -),
            B::Mul => arith!(r!(0), *acc, Mul, *),
            B::Div => arith!(r!(0), *acc, Div, /),
            B::AddSmi => arith!(*acc, Value::Number(o[0] as f64), Add, +),
            B::SubSmi => arith!(*acc, Value::Number(o[0] as f64), Sub, -),
            B::MulSmi => arith!(*acc, Value::Number(o[0] as f64), Mul, *),
            B::DivSmi => arith!(*acc, Value::Number(o[0] as f64), Div, /),
            B::Mod | B::Exp | B::BitwiseOr | B::BitwiseXor | B::BitwiseAnd | B::ShiftLeft | B::ShiftRight | B::ShiftRightLogical => {
                *acc = binary(Self::binary_op(d.bytecode), a, r!(0), *acc)?;
            }
            B::ModSmi | B::ExpSmi | B::BitwiseOrSmi | B::BitwiseXorSmi | B::BitwiseAndSmi | B::ShiftLeftSmi | B::ShiftRightSmi | B::ShiftRightLogicalSmi => {
                *acc = binary(Self::binary_op(d.bytecode), a, *acc, Value::Number(o[0] as f64))?;
            }
            B::Inc => *acc = if let Some(x) = acc.as_number() { (x + 1.0).into() } else { a.increment(*acc, 1)? },
            B::Dec => *acc = if let Some(x) = acc.as_number() { (x - 1.0).into() } else { a.increment(*acc, -1)? },
            B::Negate => *acc = a.unary(j8framework::UnaryOp::Neg, *acc)?,
            B::BitwiseNot => *acc = a.unary(j8framework::UnaryOp::BitNot, *acc)?,
            B::TypeOf => *acc = a.unary(j8framework::UnaryOp::Typeof, *acc)?,
            B::LogicalNot | B::ToBooleanLogicalNot => *acc = Value::Bool(!a.to_boolean(*acc)),
            B::ToBoolean => *acc = Value::Bool(a.to_boolean(*acc)),
            B::ToNumber => {
                if !acc.is_number() {
                    *acc = a.to_number(*acc)?.into();
                }
            }
            B::ToNumeric => {
                if !acc.is_number() {
                    *acc = match a.to_numeric(*acc)? {
                        j8framework::Numeric::Number(x) => x.into(),
                        j8framework::Numeric::BigInt(b) => Value::BigInt(b),
                    }
                }
            }
            B::ToName => {
                if !matches!(acc.kind(), ValueKind::Symbol(_)) {
                    *acc = a.property_key(*acc)?.into();
                }
            }
            B::ToString => *acc = a.to_string(*acc)?.into(),
            B::ToObject => {
                let ob = a.to_object(*acc)?;
                Self::set_reg(regs, f, o[0], ob.into());
            }
            B::TestLessThan => relational!(Lt, <),
            B::TestGreaterThan => relational!(Gt, >),
            B::TestLessThanOrEqual => relational!(Le, <=),
            B::TestGreaterThanOrEqual => relational!(Ge, >=),
            B::TestEqual | B::TestEqualStrict => {
                let op = if d.bytecode == B::TestEqual { CompareOp::Eq } else { CompareOp::StrictEq };
                *acc = compare(op, a, r!(0), *acc)?;
            }
            B::TestReferenceEqual => *acc = Value::Bool(r!(0).bits() == acc.bits()),
            B::TestInstanceOf => *acc = Value::Bool(a.instance_of(r!(0), *acc)?),
            B::TestIn => *acc = Value::Bool(a.has_keyed(*acc, r!(0))?),
            B::TestUndetectable => *acc = Value::Bool(acc.is_nullish()),
            B::TestNull => *acc = Value::Bool(matches!(acc.kind(), ValueKind::Null)),
            B::TestUndefined => *acc = Value::Bool(matches!(acc.kind(), ValueKind::Undefined)),
            B::TestTypeOf => {
                let want = ["number", "string", "symbol", "boolean", "bigint", "undefined", "function", "object"];
                let t = a.type_of(*acc);
                *acc = Value::Bool(want.get(usize::try_from(o[0]).unwrap_or(99)).is_some_and(|w| *w == t));
            }
            B::Jump | B::JumpConstant => f.pc = d.target as usize,
            B::JumpIfTrue | B::JumpIfTrueConstant | B::JumpIfFalse | B::JumpIfFalseConstant | B::JumpIfToBooleanTrue | B::JumpIfToBooleanTrueConstant | B::JumpIfToBooleanFalse | B::JumpIfToBooleanFalseConstant | B::JumpIfNull | B::JumpIfNullConstant | B::JumpIfNotNull | B::JumpIfNotNullConstant | B::JumpIfUndefined | B::JumpIfUndefinedConstant | B::JumpIfNotUndefined | B::JumpIfNotUndefinedConstant | B::JumpIfUndefinedOrNull | B::JumpIfUndefinedOrNullConstant | B::JumpIfJSReceiver | B::JumpIfJSReceiverConstant => {
                let take = match d.bytecode {
                    B::JumpIfTrue | B::JumpIfTrueConstant => matches!(acc.kind(), ValueKind::Bool(true)),
                    B::JumpIfFalse | B::JumpIfFalseConstant => matches!(acc.kind(), ValueKind::Bool(false)),
                    B::JumpIfToBooleanTrue | B::JumpIfToBooleanTrueConstant => a.to_boolean(*acc),
                    B::JumpIfToBooleanFalse | B::JumpIfToBooleanFalseConstant => !a.to_boolean(*acc),
                    B::JumpIfNull | B::JumpIfNullConstant => matches!(acc.kind(), ValueKind::Null),
                    B::JumpIfNotNull | B::JumpIfNotNullConstant => !matches!(acc.kind(), ValueKind::Null),
                    B::JumpIfUndefined | B::JumpIfUndefinedConstant => matches!(acc.kind(), ValueKind::Undefined),
                    B::JumpIfNotUndefined | B::JumpIfNotUndefinedConstant => !matches!(acc.kind(), ValueKind::Undefined),
                    B::JumpIfUndefinedOrNull | B::JumpIfUndefinedOrNullConstant => acc.is_nullish(),
                    _ => acc.as_object().is_some(),
                };
                if take {
                    f.pc = d.target as usize;
                }
            }
            B::JumpLoop => {
                f.pc = d.target as usize;
                if budget.spent() {
                    // Stop before the loop header, the state intact
                    return Ok(Flow::Done(Outcome::Paused(Box::new(State { regs: core::mem::take(regs), frames: core::mem::take(frames), acc: *acc, message: *message }))));
                }
            }
            B::SwitchOnSmiNoFeedback => {
                if let Some(x) = acc.as_number() {
                    let (table, size, base) = (o[0], o[1], o[2]);
                    #[expect(clippy::cast_possible_truncation, reason = "a case index")]
                    let i = x as i64 - base;
                    if x.fract() == 0.0 && (0..size).contains(&i)
                        && let Some(Constant::Offset(off)) = loaded.func.bytecode.constants.get(usize::try_from(table + i).unwrap_or(usize::MAX))
                    {
                        f.pc = loaded.index_of(d.start as usize + *off as usize);
                    }
                }
            }
            B::CallProperty0 | B::CallProperty1 | B::CallProperty2 | B::CallUndefinedReceiver0 | B::CallUndefinedReceiver1 | B::CallUndefinedReceiver2 => {
                let callee = r!(0);
                let (this, buf, n): (Value<'gc>, [Value<'gc>; 2], usize) = match d.bytecode {
                    B::CallProperty0 => (r!(1), [Value::Undefined; 2], 0),
                    B::CallProperty1 => (r!(1), [r!(2), Value::Undefined], 1),
                    B::CallProperty2 => (r!(1), [r!(2), r!(3)], 2),
                    B::CallUndefinedReceiver0 => (Value::Undefined, [Value::Undefined; 2], 0),
                    B::CallUndefinedReceiver1 => (Value::Undefined, [r!(1), Value::Undefined], 1),
                    _ => (Value::Undefined, [r!(1), r!(2)], 2),
                };
                return self.call_from(a, st, callee, this, Args::Values(&buf[..n]), level + 1);
            }
            B::CallProperty | B::CallAnyReceiver | B::CallUndefinedReceiver => {
                let callee = r!(0);
                let first = Self::slot_of(f, o[1]).unwrap_or(regs.len());
                let count = usize::try_from(o[2]).unwrap_or(0);
                let (this, args) = match (d.bytecode, count) {
                    (B::CallUndefinedReceiver, _) => (Value::Undefined, Args::Regs { first, count }),
                    (_, 0) => (Value::Undefined, Args::Values(&[])),
                    _ => (regs[first], Args::Regs { first: first + 1, count: count - 1 }),
                };
                return self.call_from(a, st, callee, this, args, level + 1);
            }
            B::CallWithSpread => {
                let callee = r!(0);
                let mut list = Self::reg_list(regs, f, o[1], o[2]);
                let spread = list.pop().unwrap_or_default();
                let (this, mut args) = list.split_first().map_or((Value::Undefined, Vec::new()), |(t, rest)| (*t, rest.to_vec()));
                args.extend(Self::spread(a, spread)?);
                return self.call_from(a, st, callee, this, Args::Values(&args), level + 1);
            }
            B::Construct | B::ConstructWithSpread => {
                let callee = r!(0);
                let mut args = Self::reg_list(regs, f, o[1], o[2]);
                if d.bytecode == B::ConstructWithSpread {
                    let spread = args.pop().unwrap_or_default();
                    args.extend(Self::spread(a, spread)?);
                }
                let nt = *acc;
                return self.construct_from(a, st, callee, args, nt, level + 1);
            }
            B::CallRuntime | B::InvokeIntrinsic => {
                let args = Self::reg_list(regs, f, o[1], o[2]);
                st.acc = if d.bytecode == B::InvokeIntrinsic {
                    let i = Intrinsic::from_u8(u8::try_from(o[0]).unwrap_or(u8::MAX)).ok_or_else(|| a.throw(ErrorKind::Type, "unknown intrinsic"))?;
                    self.intrinsic(a, i, &args)?
                } else {
                    let rt = Runtime::from_u16(u16::try_from(o[0]).unwrap_or(u16::MAX)).ok_or_else(|| a.throw(ErrorKind::Type, "unknown runtime function"))?;
                    self.runtime(a, st, loaded, rt, &args)?
                };
            }
            B::CreateClosure => {
                let Some(Constant::Function(n)) = loaded.func.bytecode.constants.get(usize::try_from(o[0]).unwrap_or(0)) else { unreachable!("a function constant") };
                let closure = Closure::new(FuncId(loaded.base + n), f.env(regs));
                *acc = self.function_object(a, closure).into();
            }
            B::CreateEmptyObjectLiteral => *acc = a.object().into(),
            B::CreateObjectLiteral => *acc = Self::object_literal(a, loaded, o[0]).into(),
            B::CreateEmptyArrayLiteral => *acc = a.array(Vec::new()).into(),
            B::CreateArrayLiteral => {
                let Some(Constant::Array(b)) = loaded.func.bytecode.constants.get(usize::try_from(o[0]).unwrap_or(0)) else { unreachable!("an array boilerplate") };
                if b.elements.iter().any(|e| matches!(e, Some(LiteralValue::Hole))) {
                    let arr = a.array(Vec::new());
                    for (i, e) in b.elements.iter().enumerate() {
                        if let Some(v) = e.as_ref().filter(|e| **e != LiteralValue::Hole) {
                            let v = Self::literal(a, v);
                            a.define_keyed(arr, Value::Number(i as f64), Property::data(v))?;
                        }
                    }
                    a.put_named(arr.into(), "length", Value::Number(b.elements.len() as f64), true)?;
                    *acc = arr.into();
                } else {
                    let items = b.elements.iter().map(|e| e.as_ref().map_or(Value::Undefined, |v| Self::literal(a, v))).collect();
                    *acc = a.array(items).into();
                }
            }
            B::CreateRegExpLiteral => {
                let pattern = Self::name_at(loaded, o[0]);
                let bits = u16::try_from(o[2]).unwrap_or(0);
                let flags: alloc::string::String = [('d', 1 << 7), ('g', 1), ('i', 1 << 1), ('m', 1 << 2), ('s', 1 << 5), ('u', 1 << 4), ('v', 1 << 8), ('y', 1 << 3)].iter().filter(|(_, b)| bits & b != 0).map(|(c, _)| *c).collect();
                let (p, fl) = (a.string(pattern), a.string(&flags));
                let ctor = a.realm.regexp_ctor;
                *acc = a.call(ctor.into(), Value::Undefined, &[p.into(), fl.into()])?;
            }
            B::GetTemplateObject => {
                let key = (f.func.0, u32::try_from(o[0]).unwrap_or(0));
                let cached = a.realm.templates.get().get(&key).copied();
                *acc = match cached {
                    Some(t) => t.into(),
                    None => {
                        let Some(Constant::Template(t)) = loaded.func.bytecode.constants.get(usize::try_from(o[0]).unwrap_or(0)) else { unreachable!("a template description") };
                        let cooked: Vec<Value<'gc>> = t.cooked.iter().map(|c| c.as_ref().map_or(Value::Undefined, |s| a.string(s).into())).collect();
                        let raw: Vec<Value<'gc>> = t.raw.iter().map(|s| a.string(s).into()).collect();
                        let cooked = a.array(cooked);
                        let raw = a.array(raw);
                        let frozen = |v: Value<'gc>| Property { slot: Slot::Data(v), enumerable: false, writable: false, configurable: false };
                        cooked.borrow_mut(a.mc).define("raw", frozen(raw.into()));
                        a.realm.templates.update(a.mc, |m| {
                            let mut m = m.clone();
                            m.insert(key, cooked);
                            m
                        });
                        cooked.into()
                    }
                };
            }
            B::CloneObject => {
                let ob = a.object();
                a.copy_data(ob, r!(0), &[])?;
                *acc = ob.into();
            }
            B::CreateRestParameter => {
                let formals = usize::from(loaded.func.formal_parameters);
                *acc = a.array(f.args(regs).get(formals..).unwrap_or(&[]).to_vec()).into();
            }
            B::CreateMappedArguments | B::CreateUnmappedArguments => *acc = self.arguments(a, regs, f, loaded.func.strict || d.bytecode == B::CreateUnmappedArguments)?,
            B::GetIterator => *acc = a.iterator_object(r!(0))?,
            B::ForInEnumerate => {
                let keys = a.for_in_keys(r!(0))?;
                *acc = a.array(keys).into();
            }
            B::ForInPrepare => {
                let keys = *acc;
                let len = keys.as_object().map_or(0, |k| k.elements().len());
                let first = o[0];
                Self::set_reg(regs, f, first, keys);
                Self::set_reg(regs, f, first + 1, keys);
                Self::set_reg(regs, f, first + 2, Value::Number(len as f64));
            }
            B::JumpIfForInDone | B::JumpIfForInDoneConstant => {
                let (i, n) = (r!(1).as_number().unwrap_or(0.0), r!(2).as_number().unwrap_or(0.0));
                if i >= n {
                    f.pc = d.target as usize;
                }
            }
            B::ForInNext => {
                let receiver = r!(0);
                #[expect(clippy::cast_possible_truncation, clippy::cast_sign_loss, reason = "an index")]
                let i = r!(1).as_number().unwrap_or(0.0) as usize;
                let keys = Self::reg(regs, f, o[2] + 1);
                let key = keys.as_object().and_then(|k| k.elements().get(i)).unwrap_or_default();
                // A key deleted since the enumeration started is skipped
                let still = a.has_keyed(receiver, key)?;
                *acc = if still { key } else { Value::Undefined };
            }
            B::ForInStep => {
                let i = r!(0).as_number().unwrap_or(0.0);
                Self::set_reg(regs, f, o[0], Value::Number(i + 1.0));
            }
            B::SetPendingMessage => core::mem::swap(acc, message),
            B::Throw | B::ReThrow => return Err(a.throw_value(*acc)),
            B::ThrowReferenceErrorIfHole => {
                if matches!(acc.kind(), ValueKind::Uninit) {
                    return Err(Self::uninitialized(a, Self::name_at(loaded, o[0])));
                }
            }
            B::ThrowSuperAlreadyCalledIfNotHole => {
                if !matches!(acc.kind(), ValueKind::Uninit) {
                    return Err(a.throw(ErrorKind::Reference, "Super constructor may only be called once"));
                }
            }
            B::ThrowIfNotSuperConstructor => {
                let c = r!(0);
                if !a.is_constructor(c) {
                    let what = a.describe(c);
                    return Err(a.throw(ErrorKind::Type, &alloc::format!("Super constructor {what} of anonymous class is not a constructor")));
                }
            }
            // The parent class constructor of the active function (in acc)
            B::GetSuperConstructor => {
                let parent = match acc.as_object() {
                    Some(fo) => a.get_prototype_of(fo)?.map_or(Value::Null, Value::Object),
                    None => Value::Null,
                };
                Self::set_reg(regs, f, o[0], parent);
            }
            // Default constructors are not skipped: the answer is always "not done", with the parent constructor
            B::FindNonDefaultConstructorOrConstruct => {
                let parent = match r!(0).as_object() {
                    Some(fo) => a.get_prototype_of(fo)?.map_or(Value::Null, Value::Object),
                    None => Value::Null,
                };
                Self::set_reg(regs, f, o[2], Value::Bool(false));
                Self::set_reg(regs, f, o[2] + 1, parent);
            }
            B::ConstructForwardAllArgs => {
                let callee = r!(0);
                let args = f.args(regs);
                let nt = *acc;
                return self.construct_from(a, st, callee, args, nt, level + 1);
            }
            B::GetNamedPropertyFromSuper => {
                let receiver = r!(0);
                let name = a.string(Self::name_at(loaded, o[1])).into();
                *acc = a.super_get(*acc, name, receiver)?;
            }
            B::ThrowSuperNotCalledIfHole => {
                if matches!(acc.kind(), ValueKind::Uninit) {
                    return Err(a.throw(ErrorKind::Reference, "Must call super constructor in derived class before accessing 'this' or returning from derived constructor"));
                }
            }
            B::SwitchOnGeneratorState => {
                // A call falls through; a resumption continues at its resume point, in the saved context
                if let Some(co) = r!(0).as_object() {
                    let (continuation, context) = Self::coroutine(co, |c| (c.frame.continuation, c.frame.context));
                    f.set_env(regs, context);
                    if let Some(Constant::Offset(off)) = loaded.func.bytecode.constants.get(usize::try_from(o[1] + i64::from(continuation)).unwrap_or(usize::MAX)) {
                        f.pc = loaded.index_of(d.start as usize + *off as usize);
                    }
                }
            }
            B::SuspendGenerator => {
                let Some(co) = r!(0).as_object() else { unreachable!("a generator object") };
                let first = Self::slot_of(f, o[1]).unwrap_or(regs.len());
                let count = usize::try_from(o[2]).unwrap_or(0);
                let saved = regs[first..first + count].to_vec();
                let params = regs[f.params..f.base - Frame::HEADER].to_vec();
                let context = f.env(regs);
                let continuation = u32::try_from(o[3]).unwrap_or(0);
                Self::coroutine_mut(a, co, |c| {
                    c.frame.params = params;
                    c.frame.regs = saved;
                    c.frame.context = context;
                    c.frame.continuation = continuation;
                    c.state = j8framework::CoroutineState::Suspended;
                });
                let v = *acc;
                return Ok(if st.pop_frame() {
                    st.acc = v;
                    Flow::Frame
                } else {
                    Flow::Done(Outcome::Return(v))
                });
            }
            B::ResumeGenerator => {
                let Some(co) = r!(0).as_object() else { unreachable!("a generator object") };
                let first = Self::slot_of(f, o[1]).unwrap_or(regs.len());
                let (input, saved) = Self::coroutine(co, |c| (c.input, c.frame.regs.clone()));
                regs[first..first + saved.len()].copy_from_slice(&saved);
                *acc = input;
            }
            B::Return => {
                let mut v = *acc;
                if f.slot(regs, Frame::NEW_TARGET) != Value::Undefined {
                    v = self.construct_result(a, regs, f, v)?;
                }
                return Ok(if st.pop_frame() {
                    st.acc = v;
                    Flow::Frame
                } else {
                    Flow::Done(Outcome::Return(v))
                });
            }
            b => return Err(a.throw(ErrorKind::Syntax, &alloc::format!("bytecode {b:?} is not supported by the executor yet"))),
        }
        Ok(Flow::Next)
    }

    /// The operator of an arithmetic bytecode
    fn binary_op(b: Bytecode) -> BinaryOp {
        use Bytecode as B;
        match b {
            B::Add | B::AddSmi => BinaryOp::Add,
            B::Sub | B::SubSmi => BinaryOp::Sub,
            B::Mul | B::MulSmi => BinaryOp::Mul,
            B::Div | B::DivSmi => BinaryOp::Div,
            B::Mod | B::ModSmi => BinaryOp::Rem,
            B::Exp | B::ExpSmi => BinaryOp::Exp,
            B::BitwiseOr | B::BitwiseOrSmi => BinaryOp::BitOr,
            B::BitwiseXor | B::BitwiseXorSmi => BinaryOp::BitXor,
            B::BitwiseAnd | B::BitwiseAndSmi => BinaryOp::BitAnd,
            B::ShiftLeft | B::ShiftLeftSmi => BinaryOp::Shl,
            B::ShiftRight | B::ShiftRightSmi => BinaryOp::Shr,
            _ => BinaryOp::UShr,
        }
    }

    /// The values a spread argument / element expands to (iteration)
    fn spread<'gc>(a: &mut Agent<'gc, '_>, v: Value<'gc>) -> Result<Vec<Value<'gc>>, Error> {
        a.to_list(v)
    }

    /// The frame's arguments object (unmapped: all arguments as index properties; `length`, `callee`, `@@iterator`)
    fn arguments<'gc>(&self, a: &mut Agent<'gc, '_>, regs: &[Value<'gc>], f: &Frame, strict: bool) -> Result<Value<'gc>, Error> {
        let args = f.args(regs);
        let o = ObjectData { kind: ObjectKind::Arguments(None), proto: Some(a.realm.object_proto), ..ObjectData::default() }.alloc(a.mc);
        let hidden = |v: Value<'gc>| Property { slot: Slot::Data(v), enumerable: false, writable: true, configurable: true };
        for (i, &v) in args.iter().enumerate() {
            let k = alloc::string::String::from(ryu_js::Buffer::new().format(f64::from(u32::try_from(i).unwrap_or(u32::MAX))));
            o.borrow_mut(a.mc).define(&k, Property::data(v));
        }
        let len = f64::from(u32::try_from(args.len()).unwrap_or(u32::MAX));
        let values = a.get_symbol(Value::Object(a.realm.array_proto), a.realm.symbols.iterator, Value::Undefined)?;
        let mut dd = o.borrow_mut(a.mc);
        dd.define("length", hidden(len.into()));
        if strict {
            let thrower = a.realm.named_native(a.mc, Self::throw_type_error, "", 0);
            dd.define("callee", Property { slot: Slot::Accessor { get: Some(thrower), set: Some(thrower) }, enumerable: false, writable: false, configurable: false });
        } else {
            dd.define("callee", hidden(f.slot(regs, Frame::CLOSURE)));
        }
        dd.define_symbol(a.realm.symbols.iterator, hidden(values));
        drop(dd);
        Ok(o.into())
    }

    /// %ThrowTypeError%: the accessor for strict-mode `arguments.callee`.
    fn throw_type_error<'gc>(a: &mut Agent<'gc, '_>, _: Value<'gc>, _: &[Value<'gc>]) -> Result<Value<'gc>, Error> {
        Err(a.throw(ErrorKind::Type, "'caller', 'callee', and 'arguments' properties may not be accessed on strict mode functions or the arguments objects for calls to them"))
    }

    /// `DefineClass(boilerplate, constructor, superclass, …keys and functions)`: links the constructor and a new
    /// prototype to the heritage, then defines the public methods and accessors (computed keys come in the arguments,
    /// and name their function); gives the prototype
    fn define_class<'gc>(a: &mut Agent<'gc, '_>, b: &j8bytecode::ClassBoilerplate, args: &[Value<'gc>]) -> Result<Value<'gc>, Error> {
        use j8bytecode::MemberKind as M;
        let arg = |n: usize| args.get(n).copied().unwrap_or_default();
        let Some(ctor) = arg(1).as_object() else { unreachable!("a class constructor") };
        let (proto_parent, ctor_parent) = a.class_heritage(arg(2))?;
        ctor.borrow_mut(a.mc).set_proto(Some(ctor_parent));
        let proto = ObjectData { proto: proto_parent, ..ObjectData::default() }.alloc(a.mc);
        ctor.borrow_mut(a.mc).define("prototype", Property { slot: Slot::Data(proto.into()), enumerable: false, writable: false, configurable: false });
        proto.borrow_mut(a.mc).define("constructor", Property::hidden(ctor.into()));
        let mut next = 3;
        for m in &b.members {
            let key = match &m.name {
                Some(n) => Value::from(a.string(n)),
                None => {
                    next += 1;
                    arg(next - 1)
                }
            };
            if m.kind == M::Field {
                continue;
            }
            let Some(fo) = arg(next).as_object() else { unreachable!("a member function") };
            next += 1;
            let target = if m.statik { ctor } else { proto };
            let prefix = match m.kind {
                M::Getter => Some("get"),
                M::Setter => Some("set"),
                M::Method | M::Field => None,
            };
            if m.name.is_none() {
                a.set_function_name(fo, key, prefix)?;
            }
            match m.kind {
                M::Getter | M::Setter => a.define_accessor_keyed(target, key, fo, m.kind == M::Setter, false)?,
                M::Method | M::Field => a.define_keyed(target, key, Property::hidden(fo.into()))?,
            }
        }
        Ok(proto.into())
    }

    /// Reads a coroutine (generator / async function object)
    fn coroutine<'gc, T>(co: JsObject<'gc>, read: impl FnOnce(&j8framework::Coroutine<'gc>) -> T) -> T {
        match &co.borrow().kind {
            ObjectKind::Coroutine(c) => read(c),
            _ => unreachable!("a generator object"),
        }
    }

    /// Changes a coroutine
    fn coroutine_mut<'gc, T>(a: &Agent<'gc, '_>, co: JsObject<'gc>, write: impl FnOnce(&mut j8framework::Coroutine<'gc>) -> T) -> T {
        match &mut co.borrow_mut(a.mc).kind {
            ObjectKind::Coroutine(c) => write(c),
            _ => unreachable!("a generator object"),
        }
    }

    /// `InvokeIntrinsic`
    fn intrinsic<'gc>(&self, a: &mut Agent<'gc, '_>, i: Intrinsic, args: &[Value<'gc>]) -> Result<Value<'gc>, Error> {
        let arg = |n: usize| args.get(n).copied().unwrap_or_default();
        match i {
            Intrinsic::CopyDataProperties => {
                if let Some(t) = arg(0).as_object() {
                    a.copy_data(t, arg(1), &[])?;
                }
                Ok(arg(0))
            }
            Intrinsic::CopyDataPropertiesWithExcludedPropertiesOnStack => {
                let src = arg(0);
                if src.is_nullish() {
                    return Err(a.throw(ErrorKind::Type, "Cannot destructure 'undefined' or 'null'"));
                }
                let exclude = args.iter().skip(1).filter(|k| !matches!(k.kind(), ValueKind::Symbol(_))).map(|&k| a.property_key(k)).collect::<Result<Vec<_>, _>>()?;
                let ob = a.object();
                a.copy_data(ob, src, &exclude)?;
                Ok(ob.into())
            }
            Intrinsic::CreateJSGeneratorObject => {
                // The generator's prototype is the function's `prototype` (when an object)
                let closure = arg(0);
                let asynchronous = closure.as_object().is_some_and(|o| matches!(&o.borrow().kind, ObjectKind::Function(c) if self.function(c.func).asynchronous));
                let proto = match a.get_named(closure, "prototype")?.as_object() {
                    Some(p) => p,
                    None if asynchronous => a.realm.async_generator_proto,
                    None => a.realm.generator_proto,
                };
                let co = j8framework::Coroutine::new(closure, None, asynchronous);
                Ok(ObjectData { kind: ObjectKind::Coroutine(Box::new(co)), proto: Some(proto), ..ObjectData::default() }.alloc(a.mc).into())
            }
            Intrinsic::AsyncFunctionEnter => {
                let promise = a.new_promise();
                let co = j8framework::Coroutine::new(arg(0), Some(promise), false);
                Ok(ObjectData { kind: ObjectKind::Coroutine(Box::new(co)), proto: Some(a.realm.object_proto), ..ObjectData::default() }.alloc(a.mc).into())
            }
            // `await`: the coroutine resumes once the value settles; the frame gives back the promise (what the call
            // returns when this is the first suspension)
            Intrinsic::AsyncFunctionAwait => {
                let Some(co) = arg(0).as_object() else { unreachable!("a generator object") };
                let p = a.promise_resolve(arg(1))?;
                a.perform_then(p, j8framework::Reaction::Await { coroutine: co });
                Ok(Self::coroutine(co, |c| c.promise).map_or(Value::Undefined, Value::Object))
            }
            Intrinsic::AsyncFunctionResolve | Intrinsic::AsyncFunctionReject => {
                let Some(co) = arg(0).as_object() else { unreachable!("a generator object") };
                let Some(promise) = Self::coroutine(co, |c| c.promise) else { unreachable!("an async function's promise") };
                if i == Intrinsic::AsyncFunctionResolve {
                    a.resolve_promise(promise, arg(1))?;
                } else {
                    a.reject_promise(promise, arg(1));
                }
                Ok(promise.into())
            }
            Intrinsic::CreateAsyncFromSyncIterator => {
                let iterator = arg(0);
                let next = a.get_named(iterator, "next")?;
                let rec = j8framework::IterRecord { iterator, next };
                Ok(ObjectData { kind: ObjectKind::AsyncFromSyncIterator(rec), proto: Some(a.realm.async_from_sync_iterator_proto), ..ObjectData::default() }.alloc(a.mc).into())
            }
            Intrinsic::GeneratorGetResumeMode => {
                let Some(co) = arg(0).as_object() else { unreachable!("a generator object") };
                Ok(Value::Number(Self::coroutine(co, |c| if matches!(c.waiting, j8framework::Waiting::Rethrow) { RESUME_RETHROW } else { f64::from(c.mode) })))
            }
            // Async generators: the body suspends after each of these, and `resume` turns what they recorded into the
            // step the request queue acts on
            Intrinsic::AsyncGeneratorAwait | Intrinsic::AsyncGeneratorYieldWithAwait => {
                let Some(co) = arg(0).as_object() else { unreachable!("a generator object") };
                let v = arg(1);
                let waiting = if i == Intrinsic::AsyncGeneratorAwait { j8framework::Waiting::Await(v) } else { j8framework::Waiting::Yield(v) };
                Self::coroutine_mut(a, co, |c| c.waiting = waiting);
                Ok(Value::Undefined)
            }
            Intrinsic::AsyncGeneratorResolve | Intrinsic::AsyncGeneratorReject => {
                let Some(co) = arg(0).as_object() else { unreachable!("a generator object") };
                let v = arg(1);
                let settlement = if i == Intrinsic::AsyncGeneratorResolve { j8framework::Settlement::Resolved(v) } else { j8framework::Settlement::Rejected(v) };
                // The rejection stands: the return after the body's catch handler passes through `AsyncGeneratorResolve`
                Self::coroutine_mut(a, co, |c| {
                    if !matches!(c.settlement, j8framework::Settlement::Rejected(_)) {
                        c.settlement = settlement;
                    }
                });
                Ok(Value::Undefined)
            }
            // What `next` / `return` / `throw` give for a `yield` (`yield*` passes its iterator's results on unchanged)
            Intrinsic::GeneratorYieldResult => Ok(a.iter_result(arg(0), false).into()),
            Intrinsic::GeneratorClose => {
                if let Some(co) = arg(0).as_object() {
                    Self::coroutine_mut(a, co, |c| c.state = j8framework::CoroutineState::Completed);
                }
                Ok(Value::Undefined)
            }
            other => Err(a.throw(ErrorKind::Syntax, &alloc::format!("intrinsic {other:?} is not supported yet"))),
        }
    }

    /// `CallRuntime`
    #[expect(clippy::too_many_lines, reason = "one arm per runtime function")]
    fn runtime<'gc>(&self, a: &mut Agent<'gc, '_>, st: &mut State<'gc>, loaded: &Loaded, rt: Runtime, args: &[Value<'gc>]) -> Result<Value<'gc>, Error> {
        let arg = |n: usize| args.get(n).copied().unwrap_or_default();
        let text = |a: &mut Agent<'gc, '_>, v: Value<'gc>| -> Result<alloc::string::String, Error> { Ok(alloc::string::String::from(&**a.to_string(v)?)) };
        match rt {
            Runtime::DeclareGlobals => {
                // The declarations constant is passed as the constant's index loaded by `LdaConstant` (an undefined value
                // here): find it in the constant pool
                let decls = loaded.func.bytecode.constants.iter().find_map(|c| match c {
                    Constant::Declarations(d) => Some(d.clone()),
                    _ => None,
                }).unwrap_or_default();
                self.declare_globals(a, st.f(), st.f().env(&st.regs), &decls)?;
                Ok(Value::Undefined)
            }
            Runtime::DeclareEvalVar => {
                let name = text(a, arg(0))?;
                self.declare_eval(a, st.f().env(&st.regs), &name, None)?;
                Ok(Value::Undefined)
            }
            Runtime::DeclareEvalFunction => {
                let name = text(a, arg(0))?;
                self.declare_eval(a, st.f().env(&st.regs), &name, Some(arg(1)))?;
                Ok(Value::Undefined)
            }
            Runtime::ThrowConstAssignError => Err(a.throw(ErrorKind::Type, "Assignment to constant variable.")),
            Runtime::DefineClass => {
                // The boilerplate's index in the constant pool (see `LdaConstant`)
                #[expect(clippy::cast_possible_truncation, clippy::cast_sign_loss, reason = "a constant pool index")]
                let k = arg(0).as_number().map_or(usize::MAX, |k| k as usize);
                let Some(Constant::Class(b)) = loaded.func.bytecode.constants.get(k) else { unreachable!("a class boilerplate") };
                Self::define_class(a, b, args)
            }
            Runtime::CreatePrivateNameSymbol | Runtime::CreatePrivateBrandSymbol => {
                let desc = text(a, arg(0))?;
                Ok(Value::Symbol(a.private_name(&desc)))
            }
            Runtime::CreatePrivateAccessors => Ok(ObjectData { kind: ObjectKind::AccessorPair { get: arg(0).as_object(), set: arg(1).as_object() }, ..ObjectData::default() }.alloc(a.mc).into()),
            Runtime::LoadPrivateGetter | Runtime::LoadPrivateSetter => Ok(match arg(0).as_object().map(|o| match o.borrow().kind {
                ObjectKind::AccessorPair { get, set } => if rt == Runtime::LoadPrivateGetter { get } else { set },
                _ => None,
            }) {
                Some(Some(fo)) => fo.into(),
                _ => Value::Undefined,
            }),
            Runtime::LoadFromSuper | Runtime::LoadKeyedFromSuper => a.super_get(arg(1), arg(2), arg(0)),
            Runtime::StoreToSuper | Runtime::StoreKeyedToSuper => {
                a.super_set(arg(1), arg(2), arg(3), arg(0))?;
                Ok(arg(3))
            }
            Runtime::ThrowUnsupportedSuperError => Err(a.throw(ErrorKind::Reference, "Unsupported reference to 'super'")),
            Runtime::ThrowStaticPrototypeError => Err(a.throw(ErrorKind::Type, "Classes may not have a static property named 'prototype'")),
            Runtime::ThrowTypeError => {
                let m = text(a, arg(0))?;
                Err(a.throw(ErrorKind::Type, &m))
            }
            Runtime::ThrowSymbolAsyncIteratorInvalid => Err(a.throw(ErrorKind::Type, "Result of the Symbol.asyncIterator method is not an object")),
            Runtime::ThrowThrowMethodMissing => Err(a.throw(ErrorKind::Type, "The iterator does not provide a 'throw' method")),
            Runtime::ThrowIteratorResultNotAnObject => {
                let what = a.describe(arg(0));
                Err(a.throw(ErrorKind::Type, &alloc::format!("Iterator result {what} is not an object")))
            }
            Runtime::ThrowPatternAssignmentNonCoercible => {
                let what = a.describe(arg(0));
                Err(a.throw(ErrorKind::Type, &alloc::format!("Cannot destructure '{what}' as it is undefined or null.")))
            }
            Runtime::ResolvePossiblyDirectEval => {
                let (callee, source) = (arg(0), arg(1));
                let real = matches!(callee.kind(), ValueKind::Object(o) if Gc::ptr_eq(o, a.realm.eval));
                let ValueKind::String(s) = source.kind() else { return Ok(callee) };
                if !real {
                    return Ok(callee);
                }
                let strict = arg(3).as_number().is_some_and(|x| x != 0.0);
                let src = alloc::string::String::from(&**s);
                let module = match Module::compile_direct(&j8framework::Source { text: &src, file: "[eval]" }, strict) {
                    Ok(m) => m,
                    Err(Error::Syntax(m)) => return Err(a.throw(ErrorKind::Syntax, &m)),
                    Err(e) => return Err(e),
                };
                let main = self.load(module);
                let closure = Closure::new(main, st.f().env(&st.regs));
                Ok(self.function_object(a, closure).into())
            }
            Runtime::DeleteLookupSlot => {
                let name = text(a, arg(0))?;
                Ok(Value::Bool(match self.lookup(a, st.f().env(&st.regs), &name)? {
                    Found::Slot(..) => false,
                    Found::Object(o) => a.delete(Value::Object(o), &name)?,
                    Found::Nothing => true,
                }))
            }
            Runtime::DefineAccessorPropertyUnchecked => {
                // The key is a property key already (a computed one may be a symbol)
                if let Some(o) = arg(0).as_object() {
                    if let Some(g) = arg(2).as_object() {
                        a.define_accessor_keyed(o, arg(1), g, false, true)?;
                    }
                    if let Some(s) = arg(3).as_object() {
                        a.define_accessor_keyed(o, arg(1), s, true, true)?;
                    }
                }
                Ok(Value::Undefined)
            }
            Runtime::InternalSetPrototype => {
                if let Some(o) = arg(0).as_object() {
                    match arg(1).kind() {
                        ValueKind::Object(p) => o.set_proto(a.mc, Some(p)),
                        ValueKind::Null => o.set_proto(a.mc, None),
                        _ => {}
                    }
                }
                Ok(arg(0))
            }
            other => Err(a.throw(ErrorKind::Syntax, &alloc::format!("runtime function {other:?} is not supported yet"))),
        }
    }

    /// A new execution state entering frame `f`, on a frame stack from the spares
    fn state<'gc>(&self, regs: Vec<Value<'gc>>, f: Frame) -> State<'gc> {
        let frames = self.spare_frames.borrow_mut().pop().unwrap_or_else(|| Vec::with_capacity(FRAMES_RESERVED));
        State::new(regs, frames, f)
    }

    /// A finished execution's frame stack and register file go back to the spares
    fn recycle<'gc>(&self, a: &mut Agent<'gc, '_>, st: State<'gc>) {
        let State { regs, mut frames, .. } = st;
        a.give_regs(regs);
        frames.clear();
        let mut spare = self.spare_frames.borrow_mut();
        if spare.len() < SPARE_FRAMES {
            spare.push(frames);
        }
    }

    /// Runs one frame to return in a fresh execution state.
    fn run_frame<'gc>(&self, a: &mut Agent<'gc, '_>, regs: Vec<Value<'gc>>, f: Frame, level: usize) -> Result<Value<'gc>, Error> {
        let saved = self.depth.replace(level);
        let r = self.execute(a, self.state(regs, f), &mut None).map(Outcome::returned);
        self.depth.set(saved);
        r
    }
}

/// Where a lookup by name found the name.
enum Found<'gc> {
    /// A context slot (its index in the `Env`, whether it is `const`)
    Slot(JsEnv<'gc>, usize, bool),
    /// An object's property
    Object(JsObject<'gc>),
    Nothing,
}

impl<'gc> Invoke<'gc> for Executor {
    fn call(&self, a: &mut Agent<'gc, '_>, callee: Value<'gc>, this: Value<'gc>, args: &[Value<'gc>]) -> Result<Value<'gc>, Error> {
        let level = self.depth.get() + 1;
        if level >= STACK_LIMIT {
            return Err(a.throw(ErrorKind::Range, "Maximum call stack size exceeded"));
        }
        match self.resolve(a, callee, this, args.to_vec(), level)? {
            Callee::Done(v) => Ok(v),
            Callee::Closure { closure, callee, this, args } => {
                let mut regs = a.take_regs();
                regs.clear();
                let f = self.open(a, &mut regs, closure, callee, this, &args);
                self.run_frame(a, regs, f, level)
            }
        }
    }

    fn constructs(&self, closure: &Closure<'gc>) -> bool {
        let loaded = self.function(closure.func);
        loaded.constructs || loaded.class_constructor
    }

    fn eval_code(&self, a: &mut Agent<'gc, '_>, src: &str, kind: j8framework::CodeKind) -> Result<Value<'gc>, Error> {
        let module = match Module::compile(&j8framework::Source { text: src, file: "[eval]" }, kind) {
            Ok(m) => m,
            Err(Error::Syntax(m)) => return Err(a.throw(ErrorKind::Syntax, &m)),
            Err(e) => return Err(e),
        };
        let main = self.load(module);
        let level = self.depth.get() + 1;
        if level >= STACK_LIMIT {
            return Err(a.throw(ErrorKind::Range, "Maximum call stack size exceeded"));
        }
        let saved = self.depth.replace(level);
        let r = self.run_script(a, main);
        self.depth.set(saved);
        r
    }

    /// Re-enters the generator's function with its receiver and parameters: `SwitchOnGeneratorState` jumps to the
    /// resume point, `ResumeGenerator` brings the registers back. Stopping at `SuspendGenerator` gives
    /// [`Step::Yield`] with what the frame returned, returning finishes the coroutine.
    fn resume(&self, a: &mut Agent<'gc, '_>, co: JsObject<'gc>, mode: Resume, value: Value<'gc>) -> Result<Step<'gc>, Error> {
        use j8framework::{CoroutineState as S, Settlement, Waiting as W};
        let level = self.depth.get() + 1;
        if level >= STACK_LIMIT {
            return Err(a.throw(ErrorKind::Range, "Maximum call stack size exceeded"));
        }
        let (state, waiting, async_generator) = Self::coroutine_mut(a, co, |c| (c.state, core::mem::replace(&mut c.waiting, W::Nothing), c.requests.is_some()));
        // What a suspended async generator waited for comes back first
        let (mut mode, mut rethrow) = (mode, false);
        match waiting {
            // The yielded value settled: a fulfillment answers the request, a rejection is thrown at the `yield`
            W::Yield(_) if mode == Resume::Next => return Ok(Step::Yield(value)),
            W::Yield(_) => rethrow = true,
            // The value of a `return` request settled
            W::Return => mode = if mode == Resume::Next { Resume::Return } else { Resume::Throw },
            W::Nothing | W::Await(_) | W::Rethrow => {
                // A `return` request awaits its value before the body sees it
                if async_generator && mode == Resume::Return && state != S::Running {
                    Self::coroutine_mut(a, co, |c| c.waiting = W::Return);
                    return Ok(Step::Await(value));
                }
            }
        }
        if state == S::Suspended {
            Self::coroutine_mut(a, co, |c| {
                c.state = S::Running;
                c.mode = mode;
                c.input = value;
                c.settlement = Settlement::Nothing;
                if rethrow {
                    c.waiting = W::Rethrow;
                }
            });
        }
        match (state, mode) {
            (S::Suspended, _) => {}
            (S::Running, _) => return Err(a.throw(ErrorKind::Type, "Generator is already running")),
            (S::Completed, Resume::Next) => return Ok(Step::Done(Value::Undefined)),
            (S::Completed, Resume::Return) => return Ok(Step::Done(value)),
            (S::Completed, Resume::Throw) => return Err(a.throw_value(value)),
        }
        let (callee, params) = Self::coroutine(co, |c| (c.frame.closure, c.frame.params.clone()));
        let Some(closure) = callee.as_object().and_then(|o| match &o.borrow().kind {
            ObjectKind::Function(c) => Some(*c),
            _ => None,
        }) else {
            unreachable!("a generator's function")
        };
        let mut regs = a.take_regs();
        regs.clear();
        let (this, args) = params.split_first().map_or((Value::Undefined, &[][..]), |(t, rest)| (*t, rest));
        let f = self.open(a, &mut regs, closure, callee, this, args);
        if let Some(r) = self.function(closure.func).func.bytecode.new_target_or_generator {
            regs[f.base + usize::try_from(r).unwrap_or(0)] = co.into();
        }
        let r = self.run_frame(a, regs, f, level);
        // An async generator's body reports through its intrinsics: an `await` (also of a yielded value), a request
        // answered (`yield*` / `return`), a rejection
        let v = match r {
            Ok(v) => v,
            Err(e) => {
                Self::coroutine_mut(a, co, |c| c.state = S::Completed);
                return Err(e);
            }
        };
        let (suspended, waiting, settlement) = Self::coroutine_mut(a, co, |c| {
            let suspended = c.state == S::Suspended;
            if !suspended {
                c.state = S::Completed;
            }
            (suspended, c.waiting, core::mem::replace(&mut c.settlement, Settlement::Nothing))
        });
        match (suspended, waiting, settlement) {
            (true, W::Await(w) | W::Yield(w), _) => Ok(Step::Await(w)),
            (true, _, Settlement::Resolved(x)) => Ok(Step::Yield(x)),
            (true, ..) => Ok(Step::Yield(v)),
            (false, _, Settlement::Rejected(e)) => Err(a.throw_value(e)),
            (false, _, Settlement::Resolved(x)) => Ok(Step::Done(x)),
            (false, ..) => Ok(Step::Done(v)),
        }
    }

    fn construct(&self, a: &mut Agent<'gc, '_>, callee: Value<'gc>, args: &[Value<'gc>], new_target: Value<'gc>) -> Result<Value<'gc>, Error> {
        let level = self.depth.get() + 1;
        if level >= STACK_LIMIT {
            return Err(a.throw(ErrorKind::Range, "Maximum call stack size exceeded"));
        }
        let mut regs = Vec::new();
        match self.prepare(a, &mut regs, callee, args.to_vec(), new_target, level)? {
            Prepared::Done(v) => Ok(v),
            Prepared::Frame(f) => self.run_frame(a, regs, f, level),
        }
    }
}

j8framework::paragc::brand!(
    /// The brand of a parked execution state.
    ParkedBrand = for<'gc> j8framework::GcCell<'gc, Option<State<'gc>>>
);

/// Execution state attached to the roots between slices.
type Parked = j8framework::Root<ParkedBrand>;

/// Result of running one slice.
enum Slice {
    Done(j8framework::Global),
    Paused(Parked),
}

/// A started piece of source (the engine's [`Job`](j8framework::Engine::Job))
pub struct Job(Started);

enum Started {
    /// A script, loaded, not entered yet
    Script(FuncId),
    /// A script paused between turns, its execution state on the roots
    Paused(Parked),
    /// A module: loading, linking and evaluation run in one turn
    Module { text: alloc::string::String, file: alloc::string::String },
    /// Running, finished or failed
    Spent,
}

impl j8framework::Engine for Executor {
    type Job = Job;

    fn start(&mut self, _: &mut j8framework::Isolate<'_>, _: &j8framework::ContextRoot, src: &j8framework::Source<'_>, kind: j8framework::ScriptKind) -> Result<Job, Error> {
        Ok(Job(match kind {
            j8framework::ScriptKind::Classic => Started::Script(self.load(Module::compile(src, j8framework::CodeKind::Script)?)),
            j8framework::ScriptKind::Shared => Started::Script(self.load(match &self.cache {
                Some(cache) => cache.script(src)?,
                None => Module::compile(src, j8framework::CodeKind::Script)?,
            })),
            j8framework::ScriptKind::Module => Started::Module { text: src.text.into(), file: src.file.into() },
        }))
    }

    /// A script runs until it returns or the quota is spent; then its execution state parks on the roots. A module
    /// runs loading, linking, evaluation and microtasks to the end in one turn.
    fn resume(&mut self, isolate: &mut j8framework::Isolate<'_>, cx: &j8framework::ContextRoot, job: &mut Job, quota: &mut j8framework::Quota) -> Result<Option<j8framework::Global>, Error> {
        let this: &Executor = self;
        let started = core::mem::replace(&mut job.0, Started::Spent);
        let mut budget = Some(quota.left());
        let slice = isolate.enter(|scope| -> Result<Slice, Error> {
            let realm = scope.context_of(cx);
            let mut a = scope.agent(realm, this);
            let st = match started {
                Started::Script(main) => this.script_state(&mut a, main)?,
                Started::Paused(root) => j8framework::CellExt::borrow_mut(a.fetch(&root), a.mc).take().expect("a parked state"),
                Started::Module { text, file } => {
                    this.run_module(&mut a, &file, &text, 0)?;
                    a.run_jobs()?;
                    return Ok(Slice::Done(a.global(Value::Undefined)));
                }
                Started::Spent => unreachable!("a finished job is not resumed"),
            };
            match this.execute(&mut a, st, &mut budget)? {
                Outcome::Paused(st) => Ok(Slice::Paused(a.stash::<ParkedBrand>(a.mc.alloc(j8framework::GcCell::new(a.mc, Some(*st)))))),
                Outcome::Return(v) => {
                    a.run_jobs()?;
                    Ok(Slice::Done(a.global(v)))
                }
            }
        });
        quota.spend(quota.left() - budget.unwrap_or(0));
        match slice? {
            Slice::Done(g) => Ok(Some(g)),
            Slice::Paused(root) => {
                job.0 = Started::Paused(root);
                Ok(None)
            }
        }
    }
}
