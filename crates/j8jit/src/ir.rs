use alloc::vec::Vec;

use j8framework::{FuncId, ShapeKey};

pub type V = u32;

pub type B = u32;

pub type Fs = u32;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Rep {
    None,
    I32,
    F64,
    Ctx,
    Word,
    Ptr,
    Bool,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Cond {
    Lt,
    Le,
    Gt,
    Ge,
    Eq,
    Ne,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum IntOp {
    Add,
    Sub,
    Mul,
    Mod,
    And,
    Or,
    Xor,
    Shl,
    Sar,
    Shr,
}

impl IntOp {
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

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Helper {
    Call,
    Construct,
    GetNamed(FuncId, u32),
    SetNamed(FuncId, u32, bool),
    CreateClosure(FuncId, u32),
    CreateContext(j8framework::ShapeId, u32),
    ObjectLiteral(FuncId, u32),
    Constant(FuncId, u32),
    ModF,
    ToBoolean,
    StoreCell,
    LoadGlobal(FuncId, u32, u32, bool),
    GlobalProbe(u32),
    Binary(j8framework::BinaryOp),
    Compare(j8framework::CompareOp),
    StrictEquals,
    Alloc(u32),
    GetKeyed,
    SetKeyed(bool),
    Unary(j8framework::UnaryOp),
    LooseEquals,
    InstanceOf,
}

impl Helper {
    pub fn throws(self) -> bool {
        matches!(self, Helper::Call | Helper::Construct | Helper::GetNamed(..) | Helper::SetNamed(..) | Helper::GetKeyed | Helper::SetKeyed(_) | Helper::Unary(_) | Helper::LooseEquals | Helper::InstanceOf | Helper::LoadGlobal(..) | Helper::GlobalProbe(_) | Helper::Binary(_) | Helper::Compare(_))
    }

    pub fn rep(self) -> Rep {
        match self {
            Helper::ModF => Rep::F64,
            Helper::ToBoolean | Helper::Compare(_) | Helper::StrictEquals | Helper::LooseEquals | Helper::InstanceOf => Rep::Bool,
            Helper::Alloc(_) => Rep::Ptr,
            Helper::SetNamed(..) | Helper::SetKeyed(_) | Helper::StoreCell => Rep::None,
            _ => Rep::Word,
        }
    }
}

#[derive(Clone)]
pub enum Op {
    I32(i32),
    F64(f64),
    Word(u64),
    Param(u32),
    Callee,
    Phi(Vec<V>),
    Int(IntOp, V, V, Option<Fs>),
    Float(FloatOp, V, V),
    IntToFloat(V),
    BoxInt(V),
    BoxFloat(V),
    BoxBool(V),
    BoxPtr(V, u64),
    UnboxInt(V, Fs),
    UnboxFloat(V, Fs),
    FloatToInt(V, Fs),
    CheckObject(V, Fs),
    CheckNotHole(V, Fs),
    CheckNotLazy(V, Fs),
    CheckFunc(V, FuncId, Fs),
    CheckShape(V, Option<ShapeKey>, Fs),
    Check(V, bool, Fs),
    CmpInt(Cond, V, V),
    CmpFloat(Cond, V, V),
    CmpWord(Cond, V, V),
    LoadWord(V, i32),
    LoadPtr(V, i32),
    LoadRaw(V, i32),
    StoreCell(V, i32, V),
    StoreCellAddr(V, u32),
    ContextCell(V, u32),
    Call(Helper, Vec<V>, Option<Fs>),
    CallJit(u32, V, V, Vec<V>, Fs),
    Untag(V),
    CtxField(i32),
    Enter(Fs),
    Global(u32, Option<FuncId>, Fs),
    Poll(Fs),
    Jump(B),
    Branch(V, B, B),
    Return(V, u32),
    Deopt(Fs),
}

impl Op {
    pub fn is_terminator(&self) -> bool {
        matches!(self, Op::Jump(_) | Op::Branch(..) | Op::Return(..) | Op::Deopt(_))
    }

    pub fn inputs(&self, out: &mut Vec<V>) {
        out.clear();
        match self {
            Op::I32(_) | Op::F64(_) | Op::Word(_) | Op::Param(_) | Op::Callee | Op::CtxField(_) | Op::Poll(_) | Op::Enter(_) | Op::Global(..) | Op::Jump(_) | Op::Deopt(_) => {}
            Op::Phi(v) => out.extend_from_slice(v),
            Op::Int(_, a, b, _) | Op::Float(_, a, b) | Op::CmpInt(_, a, b) | Op::CmpFloat(_, a, b) | Op::CmpWord(_, a, b) | Op::StoreCell(a, _, b) => out.extend([*a, *b]),
            Op::IntToFloat(a) | Op::BoxInt(a) | Op::BoxFloat(a) | Op::BoxBool(a) | Op::BoxPtr(a, _) | Op::UnboxInt(a, _) | Op::UnboxFloat(a, _) | Op::FloatToInt(a, _) | Op::CheckObject(a, _) | Op::CheckNotHole(a, _) | Op::CheckNotLazy(a, _) | Op::CheckFunc(a, _, _) | Op::CheckShape(a, _, _) | Op::Check(a, _, _) | Op::LoadWord(a, _) | Op::LoadPtr(a, _) | Op::LoadRaw(a, _) | Op::StoreCellAddr(a, _) | Op::ContextCell(a, _) | Op::Untag(a) | Op::Branch(a, _, _) | Op::Return(a, _) => out.push(*a),
            Op::Call(_, args, _) => out.extend_from_slice(args),
            Op::CallJit(_, c, t, args, _) => {
                out.extend([*c, *t]);
                out.extend_from_slice(args);
            }
        }
    }

    pub fn map_inputs(&mut self, mut f: impl FnMut(V) -> V) {
        match self {
            Op::I32(_) | Op::F64(_) | Op::Word(_) | Op::Param(_) | Op::Callee | Op::CtxField(_) | Op::Poll(_) | Op::Enter(_) | Op::Global(..) | Op::Jump(_) | Op::Deopt(_) => {}
            Op::Phi(v) => v.iter_mut().for_each(|x| *x = f(*x)),
            Op::Int(_, a, b, _) | Op::Float(_, a, b) | Op::CmpInt(_, a, b) | Op::CmpFloat(_, a, b) | Op::CmpWord(_, a, b) | Op::StoreCell(a, _, b) => {
                *a = f(*a);
                *b = f(*b);
            }
            Op::IntToFloat(a) | Op::BoxInt(a) | Op::BoxFloat(a) | Op::BoxBool(a) | Op::BoxPtr(a, _) | Op::UnboxInt(a, _) | Op::UnboxFloat(a, _) | Op::FloatToInt(a, _) | Op::CheckObject(a, _) | Op::CheckNotHole(a, _) | Op::CheckNotLazy(a, _) | Op::CheckFunc(a, _, _) | Op::CheckShape(a, _, _) | Op::Check(a, _, _) | Op::LoadWord(a, _) | Op::LoadPtr(a, _) | Op::LoadRaw(a, _) | Op::StoreCellAddr(a, _) | Op::ContextCell(a, _) | Op::Untag(a) | Op::Branch(a, _, _) | Op::Return(a, _) => *a = f(*a),
            Op::Call(_, args, _) => args.iter_mut().for_each(|x| *x = f(*x)),
            Op::CallJit(_, c, t, args, _) => {
                *c = f(*c);
                *t = f(*t);
                args.iter_mut().for_each(|x| *x = f(*x));
            }
        }
    }

    pub fn frame_state(&self) -> Option<Fs> {
        match self {
            Op::Int(_, _, _, fs) | Op::Call(_, _, fs) => *fs,
            Op::UnboxInt(_, fs) | Op::UnboxFloat(_, fs) | Op::FloatToInt(_, fs) | Op::CheckObject(_, fs) | Op::CheckNotHole(_, fs) | Op::CheckNotLazy(_, fs) | Op::CheckFunc(_, _, fs) | Op::CheckShape(_, _, fs) | Op::Check(_, _, fs) | Op::CallJit(.., fs) | Op::Poll(fs) | Op::Enter(fs) | Op::Global(_, _, fs) | Op::Deopt(fs) => Some(*fs),
            _ => None,
        }
    }

    pub fn effectful(&self) -> bool {
        match self {
            Op::Int(op, _, _, fs) => fs.is_some() && !op.total(),
            Op::Call(..) | Op::CallJit(..) | Op::StoreCell(..) | Op::Poll(_) | Op::Enter(_) | Op::Global(..) => true,
            Op::UnboxInt(..) | Op::UnboxFloat(..) | Op::FloatToInt(..) | Op::CheckObject(..) | Op::CheckNotHole(..) | Op::CheckNotLazy(..) | Op::CheckFunc(..) | Op::CheckShape(..) | Op::Check(..) => true,
            o => o.is_terminator(),
        }
    }
}

#[derive(Clone)]
pub struct Inst {
    pub op: Op,
    pub rep: Rep,
    pub block: B,
}

#[derive(Clone, Default)]
pub struct Block {
    pub insts: Vec<V>,
    pub preds: Vec<B>,
    pub loop_header: bool,
    pub cold: bool,
}

#[derive(Clone, Debug)]
pub enum Slot {
    Value(V),
    Virtual(u32),
    Closure,
}

#[derive(Clone)]
pub struct VirtualObject {
    pub proto: V,
    pub shape: Option<ShapeKey>,
    pub fields: Vec<Slot>,
}

#[derive(Clone)]
pub struct Frame {
    pub func: FuncId,
    pub pc: u32,
    pub callee: Slot,
    pub params: Vec<Slot>,
    pub argc: u32,
    pub regs: Vec<Slot>,
    pub context: Slot,
    pub acc: Slot,
    pub construct: Option<(Slot, Slot)>,
}

#[derive(Clone, Default)]
pub struct FrameState {
    pub frames: Vec<Frame>,
    pub objects: Vec<VirtualObject>,
    pub writes: Vec<(V, u32, Slot)>,
}

#[derive(Default)]
pub struct Graph {
    pub hoisted: Vec<(V, Fs)>,
    pub preheaders: Vec<(B, B, Fs)>,
    pub globals: Vec<(FuncId, u32, u32)>,
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

    pub fn push(&mut self, b: B, op: Op, rep: Rep) -> V {
        let v = u32::try_from(self.insts.len()).unwrap_or(u32::MAX);
        self.insts.push(Inst { op, rep, block: b });
        self.blocks[b as usize].insts.push(v);
        v
    }

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

    pub fn succs(&self, b: B) -> Vec<B> {
        match self.blocks[b as usize].insts.last().map(|&v| &self.insts[v as usize].op) {
            Some(Op::Jump(t)) => alloc::vec![*t],
            Some(Op::Branch(_, t, f)) => alloc::vec![*t, *f],
            _ => Vec::new(),
        }
    }
}

impl Graph {
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
                    Op::CheckShape(a, sh, fs) => alloc::format!("CheckShape(v{a}, {:?}, fs{fs})", sh.map(ShapeKey::id)),
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
                    Op::CheckNotLazy(..) => "CheckNotLazy",
                    Op::Check(..) => "Check",
                    Op::StoreCellAddr(..) => "StoreCellAddr",
                    Op::Untag(_) => "Untag",
                    Op::Poll(_) => "Poll",
                    Op::Enter(_) => "Enter",
                    Op::Return(..) => "Return",
                    Op::Deopt(_) => "Deopt",
                    Op::Callee => "Callee",
                    _ => "?",
                };
                full.into()
            }
        }
    }
}
