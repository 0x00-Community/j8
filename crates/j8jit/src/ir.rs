//! The graph the JIT optimizes: basic blocks of instructions in SSA form, each instruction one value with a
//! representation ([`Rep`]). Guards carry the frame state the interpreter resumes from when they fail.
//!
//! ```text
//! Block { phis…, insts…, terminator }      a terminator is Jump / Branch / Return / Deopt
//! Inst  { op, rep }                        operands are value ids (instruction indices)
//! FrameState { func, pc, slots…, parent }  what each interpreter register holds at a guard (virtual objects too)
//! ```
use alloc::sync::Arc;
use alloc::vec::Vec;

use j8framework::{FuncId, Shape};

/// A value: the index of the instruction producing it
pub type V = u32;
/// A block's index
pub type B = u32;
/// A frame state's index
pub type Fs = u32;

/// How a value is held in machine code.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Rep {
    /// No value (stores, guards, terminators)
    None,
    /// A 32-bit integer standing for a JS number (never −0)
    I32,
    /// A JS number as a double
    F64,
    /// The address of a context (an `Env`)
    Ctx,
    /// A NaN-boxed [`j8framework::Value`] word
    Word,
    /// The address of a heap object (an object, a context): its word without the tag
    Ptr,
    /// A flag (0 / 1)
    Bool,
}

impl Rep {
    /// Held in an XMM register
    pub fn float(self) -> bool {
        self == Rep::F64
    }
}

/// Integer / float comparison.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Cond {
    Lt,
    Le,
    Gt,
    Ge,
    Eq,
    Ne,
}

impl Cond {
    pub fn negate(self) -> Cond {
        match self {
            Cond::Lt => Cond::Ge,
            Cond::Le => Cond::Gt,
            Cond::Gt => Cond::Le,
            Cond::Ge => Cond::Lt,
            Cond::Eq => Cond::Ne,
            Cond::Ne => Cond::Eq,
        }
    }
}

/// Two-operand int32 operations (guards fail on overflow, on −0 results, on a zero divisor …).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum IntOp {
    Add,
    Sub,
    Mul,
    /// `%`: deopts when the result would be −0 or the divisor is 0
    Mod,
    And,
    Or,
    Xor,
    Shl,
    Sar,
    /// `>>>`: deopts when the result doesn't fit an int32
    Shr,
}

impl IntOp {
    /// Can't fail
    pub fn total(self) -> bool {
        matches!(self, IntOp::And | IntOp::Or | IntOp::Xor | IntOp::Shl | IntOp::Sar)
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum FloatOp {
    Add,
    Sub,
    Mul,
    Div,
}

/// A runtime function called from machine code (see `runtime.rs`); all take the context first and give a word.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Helper {
    /// (callee, this, args…) ⇒ result; may throw
    Call,
    /// (callee, new.target, args…) ⇒ result; may throw
    Construct,
    /// (object word, name constant) ⇒ value; may throw
    GetNamed(FuncId, u32),
    /// (object word, value, name constant) ⇒ (); may throw
    SetNamed(FuncId, u32, bool),
    /// (context ptr) ⇒ closure word
    CreateClosure(FuncId, u32),
    /// (context ptr) ⇒ new context word (`slots` slots, `undefined`)
    CreateContext(j8framework::ShapeId, u32),
    /// () ⇒ object literal word
    ObjectLiteral(FuncId, u32),
    /// (ctor word, new.target word) ⇒ `this` word
    CreateThis(FuncId),
    /// (a, b) ⇒ a % b (doubles)
    ModF,
    /// (word) ⇒ ToBoolean as 0 / 1
    ToBoolean,
    /// (cell address, word): a write through the barrier
    StoreCell,
    /// () ⇒ value: a global name the slow way (name constant, feedback slot)
    LoadGlobal(FuncId, u32, u32),
    /// (cache address) ⇒ value: fills global cache site `k` (see `Op::Global`)
    GlobalProbe(u32),
    /// (a, b) ⇒ a op b, any values
    Binary(j8framework::BinaryOp),
    /// (a, b) ⇒ the comparison as 0 / 1
    Compare(j8framework::CompareOp),
    /// (a, b) ⇒ a === b as 0 / 1
    StrictEquals,
    /// (proto ptr, field words…) ⇒ a new ordinary object with `shape` (an index into the compilation's shapes)
    Alloc(u32),
}

impl Helper {
    /// May throw: a pending exception makes the code hand over
    pub fn throws(self) -> bool {
        matches!(self, Helper::Call | Helper::Construct | Helper::GetNamed(..) | Helper::SetNamed(..) | Helper::CreateThis(_) | Helper::LoadGlobal(..) | Helper::GlobalProbe(_) | Helper::Binary(_) | Helper::Compare(_))
    }

    /// The representation of its result
    pub fn rep(self) -> Rep {
        match self {
            Helper::ModF => Rep::F64,
            Helper::ToBoolean | Helper::Compare(_) | Helper::StrictEquals => Rep::Bool,
            Helper::Alloc(_) => Rep::Ptr,
            Helper::SetNamed(..) | Helper::StoreCell => Rep::None,
            _ => Rep::Word,
        }
    }
}

/// An instruction.
#[derive(Clone)]
pub enum Op {
    I32(i32),
    F64(f64),
    Word(u64),
    /// Slot `i` of the entry image (see [`crate::build`]): a word
    Param(u32),
    /// The function object of a native entry (a pointer word)
    Callee,
    /// One input per predecessor of its block
    Phi(Vec<V>),
    /// Int32 arithmetic; `fs` when it can fail
    Int(IntOp, V, V, Option<Fs>),
    Float(FloatOp, V, V),
    IntToFloat(V),
    /// int32 ⇒ double ⇒ word
    BoxInt(V),
    BoxFloat(V),
    BoxBool(V),
    /// An object / context address ⇒ its word
    BoxPtr(V, u64),
    /// A number word ⇒ int32 (guard: a number that is an int32 and not −0)
    UnboxInt(V, Fs),
    /// A number word ⇒ double (guard: a number)
    UnboxFloat(V, Fs),
    /// A double ⇒ int32 (guard: exact, not −0)
    FloatToInt(V, Fs),
    /// A word ⇒ the object's address (guard: an object)
    CheckObject(V, Fs),
    /// Guard: the word is not the hole
    CheckNotHole(V, Fs),
    /// Guard: the object runs function `FuncId` (a closure of it)
    CheckFunc(V, FuncId, Fs),
    /// Guard: the object has this shape (and is plain); `None`: no shape (an empty object)
    CheckShape(V, Option<Arc<Shape>>, Fs),
    /// Guard: the flag holds (`true`) / doesn't
    Check(V, bool, Fs),
    CmpInt(Cond, V, V),
    CmpFloat(Cond, V, V),
    /// The two words are the same bits
    CmpWord(Cond, V, V),
    /// The word at `base + offset`
    LoadWord(V, i32),
    /// The address held by the GC slot / pointer word at `base + offset` (its low 48 bits)
    LoadPtr(V, i32),
    /// A raw 64-bit load (`Option<Gc>` fields: 0 or an address)
    LoadRaw(V, i32),
    /// The byte at `base + offset`
    LoadByte(V, i32),
    /// The cell at `base + offset` takes the word (a barrier when an old or the new word is a pointer)
    StoreCell(V, i32, V),
    /// The inline-or-store cell address of property entry `slot` (≥ the inline ones): through the store leaf
    StoreCellAddr(V, u32),
    /// The context's slot cell address (`index` counted from the first variable)
    ContextCell(V, u32),
    /// A runtime call
    Call(Helper, Vec<V>, Option<Fs>),
    /// A call to compiled code: native entry of `code` (an index into the compilation's targets), `callee` the
    /// function object's word, `this`, the arguments
    CallJit(u32, V, V, Vec<V>, Fs),
    /// A word ⇒ the address it carries (a context's, an object's), unchecked
    Untag(V),
    /// A field of the JIT context (see `runtime::Ctx`): realm objects' addresses
    CtxField(i32),
    /// A native entry: counts the call depth; too deep hands over (the interpreter throws)
    Enter(Fs),
    /// A global name through cache site `k`: valid while nothing but compiled code ran since it was filled
    /// (`Ctx::EPOCH`); the slow path looks it up and, when `expect` is set, checks it is a closure of that function
    Global(u32, Option<FuncId>, Fs),
    /// A loop back edge: counts the budget; when spent, may hand over (resuming at the loop header)
    Poll(Fs),
    // ---- terminators ----
    Jump(B),
    Branch(V, B, B),
    /// Returns the value; `pc`: the `Return` instruction (an interpreter entry resumes there)
    Return(V, u32),
    /// Hand over to the interpreter unconditionally
    Deopt(Fs),
}

impl Op {
    pub fn is_terminator(&self) -> bool {
        matches!(self, Op::Jump(_) | Op::Branch(..) | Op::Return(..) | Op::Deopt(_))
    }

    /// The value operands
    pub fn inputs(&self, out: &mut Vec<V>) {
        out.clear();
        match self {
            Op::I32(_) | Op::F64(_) | Op::Word(_) | Op::Param(_) | Op::Callee | Op::CtxField(_) | Op::Poll(_) | Op::Enter(_) | Op::Global(..) | Op::Jump(_) | Op::Deopt(_) => {}
            Op::Phi(v) => out.extend_from_slice(v),
            Op::Int(_, a, b, _) | Op::Float(_, a, b) | Op::CmpInt(_, a, b) | Op::CmpFloat(_, a, b) | Op::CmpWord(_, a, b) | Op::StoreCell(a, _, b) => out.extend([*a, *b]),
            Op::IntToFloat(a) | Op::BoxInt(a) | Op::BoxFloat(a) | Op::BoxBool(a) | Op::BoxPtr(a, _) | Op::UnboxInt(a, _) | Op::UnboxFloat(a, _) | Op::FloatToInt(a, _) | Op::CheckObject(a, _) | Op::CheckNotHole(a, _) | Op::CheckFunc(a, _, _) | Op::CheckShape(a, _, _) | Op::Check(a, _, _) | Op::LoadWord(a, _) | Op::LoadPtr(a, _) | Op::LoadRaw(a, _) | Op::LoadByte(a, _) | Op::StoreCellAddr(a, _) | Op::ContextCell(a, _) | Op::Untag(a) | Op::Branch(a, _, _) | Op::Return(a, _) => out.push(*a),
            Op::Call(_, args, _) => out.extend_from_slice(args),
            Op::CallJit(_, c, t, args, _) => {
                out.extend([*c, *t]);
                out.extend_from_slice(args);
            }

        }
    }

    /// Rewrites the value operands
    pub fn map_inputs(&mut self, mut f: impl FnMut(V) -> V) {
        match self {
            Op::I32(_) | Op::F64(_) | Op::Word(_) | Op::Param(_) | Op::Callee | Op::CtxField(_) | Op::Poll(_) | Op::Enter(_) | Op::Global(..) | Op::Jump(_) | Op::Deopt(_) => {}
            Op::Phi(v) => v.iter_mut().for_each(|x| *x = f(*x)),
            Op::Int(_, a, b, _) | Op::Float(_, a, b) | Op::CmpInt(_, a, b) | Op::CmpFloat(_, a, b) | Op::CmpWord(_, a, b) | Op::StoreCell(a, _, b) => {
                *a = f(*a);
                *b = f(*b);
            }
            Op::IntToFloat(a) | Op::BoxInt(a) | Op::BoxFloat(a) | Op::BoxBool(a) | Op::BoxPtr(a, _) | Op::UnboxInt(a, _) | Op::UnboxFloat(a, _) | Op::FloatToInt(a, _) | Op::CheckObject(a, _) | Op::CheckNotHole(a, _) | Op::CheckFunc(a, _, _) | Op::CheckShape(a, _, _) | Op::Check(a, _, _) | Op::LoadWord(a, _) | Op::LoadPtr(a, _) | Op::LoadRaw(a, _) | Op::LoadByte(a, _) | Op::StoreCellAddr(a, _) | Op::ContextCell(a, _) | Op::Untag(a) | Op::Branch(a, _, _) | Op::Return(a, _) => *a = f(*a),
            Op::Call(_, args, _) => args.iter_mut().for_each(|x| *x = f(*x)),
            Op::CallJit(_, c, t, args, _) => {
                *c = f(*c);
                *t = f(*t);
                args.iter_mut().for_each(|x| *x = f(*x));
            }

        }
    }

    /// The frame state it hands over with, if it can
    pub fn frame_state(&self) -> Option<Fs> {
        match self {
            Op::Int(_, _, _, fs) | Op::Call(_, _, fs) => *fs,
            Op::UnboxInt(_, fs) | Op::UnboxFloat(_, fs) | Op::FloatToInt(_, fs) | Op::CheckObject(_, fs) | Op::CheckNotHole(_, fs) | Op::CheckFunc(_, _, fs) | Op::CheckShape(_, _, fs) | Op::Check(_, _, fs) | Op::CallJit(.., fs) | Op::Poll(fs) | Op::Enter(fs) | Op::Global(_, _, fs) | Op::Deopt(fs) => Some(*fs),
            _ => None,
        }
    }

    /// Has an effect beyond its value (kept even when unused)
    pub fn effectful(&self) -> bool {
        if let Op::Global(..) = self {
            return false;
        }
        match self {
            Op::Int(op, _, _, fs) => fs.is_some() && !op.total(),
            Op::Call(..) | Op::CallJit(..) | Op::StoreCell(..) | Op::Poll(_) | Op::Enter(_) => true,
            Op::UnboxInt(..) | Op::UnboxFloat(..) | Op::FloatToInt(..) | Op::CheckObject(..) | Op::CheckNotHole(..) | Op::CheckFunc(..) | Op::CheckShape(..) | Op::Check(..) => true,
            o => o.is_terminator(),
        }
    }
}

/// An instruction and its representation.
#[derive(Clone)]
pub struct Inst {
    pub op: Op,
    pub rep: Rep,
    /// Its block (set when placed)
    pub block: B,
}

/// A basic block: its instructions in order (phis first, the terminator last).
#[derive(Clone, Default)]
pub struct Block {
    pub insts: Vec<V>,
    pub preds: Vec<B>,
    /// A loop header: its back edges come from later blocks
    pub loop_header: bool,
    /// Rarely run (guards' slow paths are out of line anyway; this marks blocks after unconditional deopts)
    pub cold: bool,
}

/// What a frame-state slot holds when the interpreter takes over.
#[derive(Clone, Debug)]
pub enum Slot {
    /// A value (boxed by its representation)
    Value(V),
    /// An object not allocated yet: materialized at hand-over
    Virtual(u32),
    /// The frame's closure's own context (the hand-over takes it from the function)
    Closure,
}

/// An object the code never allocated (escape analysis): the hand-over builds it.
#[derive(Clone)]
pub struct VirtualObject {
    /// The prototype's address
    pub proto: V,
    /// `None`: no properties yet
    pub shape: Option<Arc<Shape>>,
    pub fields: Vec<Slot>,
}

/// One interpreter frame the code stands for at a guard.
#[derive(Clone)]
pub struct Frame {
    pub func: FuncId,
    /// The instruction the interpreter goes on with
    pub pc: u32,
    /// The function object
    pub callee: Slot,
    /// Receiver and formal parameters
    pub params: Vec<Slot>,
    pub argc: u32,
    /// r0 …
    pub regs: Vec<Slot>,
    pub context: Slot,
    pub acc: Slot,
    /// A construct frame: new.target and `this`
    pub construct: Option<(Slot, Slot)>,
}

/// What the interpreter continues with when a guard fails: its frames (outermost first, inlined callees after; a
/// caller's `pc` is past its call), the objects not allocated yet, and context writes not done yet.
#[derive(Clone, Default)]
pub struct FrameState {
    pub frames: Vec<Frame>,
    pub objects: Vec<VirtualObject>,
    /// (context address, slot index, value)
    pub writes: Vec<(V, u32, Slot)>,
}

/// A function's graph.
#[derive(Default)]
pub struct Graph {
    /// Loop headers entered from one block: (header, that block, the frame state of entering the loop)
    pub preheaders: Vec<(B, B, Fs)>,
    /// Global cache sites: (function, name constant, feedback slot)
    pub globals: Vec<(FuncId, u32, u32)>,
    /// Interpreter entries: the entry image's parameter and register counts
    pub entry_params: usize,
    pub entry_regs: usize,
    pub insts: Vec<Inst>,
    pub blocks: Vec<Block>,
    pub states: Vec<FrameState>,
}

impl Graph {
    pub fn add_block(&mut self) -> B {
        self.blocks.push(Block::default());
        u32::try_from(self.blocks.len() - 1).unwrap_or(u32::MAX)
    }

    /// A new instruction at the end of `b`
    pub fn push(&mut self, b: B, op: Op, rep: Rep) -> V {
        let v = u32::try_from(self.insts.len()).unwrap_or(u32::MAX);
        self.insts.push(Inst { op, rep, block: b });
        self.blocks[b as usize].insts.push(v);
        v
    }

    /// A phi at the start of `b`
    pub fn push_phi(&mut self, b: B, inputs: Vec<V>, rep: Rep) -> V {
        let v = u32::try_from(self.insts.len()).unwrap_or(u32::MAX);
        self.insts.push(Inst { op: Op::Phi(inputs), rep, block: b });
        let blk = &mut self.blocks[b as usize];
        let at = blk.insts.iter().position(|&i| !matches!(self.insts[i as usize].op, Op::Phi(_))).unwrap_or(blk.insts.len());
        blk.insts.insert(at, v);
        v
    }

    pub fn rep(&self, v: V) -> Rep {
        self.insts[v as usize].rep
    }

    pub fn op(&self, v: V) -> &Op {
        &self.insts[v as usize].op
    }

    pub fn add_state(&mut self, s: FrameState) -> Fs {
        self.states.push(s);
        u32::try_from(self.states.len() - 1).unwrap_or(u32::MAX)
    }

    /// The block ends (has its terminator)
    pub fn terminated(&self, b: B) -> bool {
        self.blocks[b as usize].insts.last().is_some_and(|&v| self.insts[v as usize].op.is_terminator())
    }

    /// Successors of a terminated block
    pub fn succs(&self, b: B) -> Vec<B> {
        match self.blocks[b as usize].insts.last().map(|&v| &self.insts[v as usize].op) {
            Some(Op::Jump(t)) => alloc::vec![*t],
            Some(Op::Branch(_, t, f)) => alloc::vec![*t, *f],
            _ => Vec::new(),
        }
    }
}

impl Graph {
    /// A readable listing (debugging)
    pub fn dump(&self) -> alloc::string::String {
        use core::fmt::Write;
        let mut s = alloc::string::String::new();
        for (b, blk) in self.blocks.iter().enumerate() {
            if blk.insts.is_empty() {
                continue;
            }
            let _ = writeln!(s, "b{b} preds {:?}{}", blk.preds, if blk.loop_header { " loop" } else { "" });
            for &v in &blk.insts {
                let i = &self.insts[v as usize];
                let op = match &i.op {
                    Op::CheckShape(a, sh, fs) => alloc::format!("CheckShape(v{a}, {:?}, fs{fs})", sh.as_ref().map(Arc::as_ptr)),
                    Op::Call(h, args, fs) => alloc::format!("Call({h:?}, {args:?}, {fs:?})"),
                    o => {
                        let mut ins = Vec::new();
                        o.inputs(&mut ins);
                        let name = alloc::format!("{:?}", core::mem::discriminant(o));
                        let _ = name;
                        alloc::format!("{} {:?}", Self::op_name(o), ins)
                    }
                };
                let _ = writeln!(s, "  v{v}: {:?} = {op}", i.rep);
            }
        }
        s
    }

    fn op_name(o: &Op) -> alloc::string::String {
        match o {
            Op::I32(x) => alloc::format!("I32({x})"),
            Op::F64(x) => alloc::format!("F64({x})"),
            Op::Word(x) => alloc::format!("Word({x:#x})"),
            Op::Param(i) => alloc::format!("Param({i})"),
            Op::Int(op, ..) => alloc::format!("Int({op:?})"),
            Op::Float(op, ..) => alloc::format!("Float({op:?})"),
            Op::LoadWord(_, o) => alloc::format!("LoadWord(+{o})"),
            Op::LoadPtr(_, o) => alloc::format!("LoadPtr(+{o})"),
            Op::LoadRaw(_, o) => alloc::format!("LoadRaw(+{o})"),
            Op::StoreCell(_, o, _) => alloc::format!("StoreCell(+{o})"),
            Op::ContextCell(_, i) => alloc::format!("ContextCell({i})"),
            Op::CheckFunc(_, f, _) => alloc::format!("CheckFunc({f:?})"),
            Op::Global(k, e, _) => alloc::format!("Global({k}, {e:?})"),
            Op::CtxField(o) => alloc::format!("CtxField({o})"),
            Op::Jump(t) => alloc::format!("Jump(b{t})"),
            Op::Branch(_, t, f) => alloc::format!("Branch(b{t}, b{f})"),
            Op::CallJit(k, ..) => alloc::format!("CallJit(t{k})"),
            Op::CmpInt(c, ..) | Op::CmpFloat(c, ..) | Op::CmpWord(c, ..) => alloc::format!("Cmp{c:?}"),
            o => {
                let s = alloc::format!("{:?}", o.frame_state());
                let _ = s;
                let full = match o {
                    Op::Phi(_) => "Phi",
                    Op::IntToFloat(_) => "IntToFloat",
                    Op::BoxInt(_) => "BoxInt",
                    Op::BoxFloat(_) => "BoxFloat",
                    Op::BoxBool(_) => "BoxBool",
                    Op::BoxPtr(..) => "BoxPtr",
                    Op::UnboxInt(..) => "UnboxInt",
                    Op::UnboxFloat(..) => "UnboxFloat",
                    Op::FloatToInt(..) => "FloatToInt",
                    Op::CheckObject(..) => "CheckObject",
                    Op::CheckNotHole(..) => "CheckNotHole",
                    Op::Check(..) => "Check",
                    Op::StoreCellAddr(..) => "StoreCellAddr",
                    Op::Untag(_) => "Untag",
                    Op::Poll(_) => "Poll",
                    Op::Enter(_) => "Enter",
                    Op::Return(..) => "Return",
                    Op::Deopt(_) => "Deopt",
                    Op::Callee => "Callee",
                    Op::LoadByte(..) => "LoadByte",
                    _ => "?",
                };
                full.into()
            }
        }
    }
}
