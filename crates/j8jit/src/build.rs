use alloc::collections::BTreeMap;
use alloc::rc::Rc;
use alloc::vec;
use alloc::vec::Vec;

use j8bytecode::{Bytecode, Constant, LiteralValue, OperandValue};
use j8executor::{Executor, JitFunction as Function, JitReg as Reg};
use j8framework::{BinaryOp, CompareOp, FuncId, Layout, Lazy, ShapeKey, ValueBits};

use crate::cfg::{Bits, Cfg};
use crate::ir::{Cond, FloatOp, Frame, FrameState, Graph, Helper, IntOp, Op, Rep, Slot, VirtualObject, B, Fs, V};
use crate::runtime::Ctx;

pub type Hints = BTreeMap<(u32, u32), u8>;

pub const HINT_FLOAT: u8 = 1;

pub const HINT_GENERIC: u8 = 2;

pub const HINT_NO_INLINE: u8 = 4;

pub const OBJECT_TAG: u64 = 0xFFF0_0000_0000_0000 | ((j8framework::ValueTag::OBJECT as u64 + 1) << 48);
pub const CONTEXT_TAG: u64 = 0xFFF0_0000_0000_0000 | ((j8framework::ValueTag::CONTEXT as u64 + 1) << 48);
pub const STRING_TAG: u64 = 0xFFF0_0000_0000_0000 | ((j8framework::ValueTag::STRING as u64 + 1) << 48);

const INLINE_DEPTH: usize = 4;

const INLINE_SIZE: usize = 80;

const LOAD_WORD: u8 = 0;
const LOAD_PTR: u8 = 1;
const LOAD_RAW: u8 = 2;

const RECURSIVE_INLINE: usize = 2;

const RESTARTS: usize = 12;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Kind {
    Enter,
    Osr(usize),
    Native(usize),
}

pub struct Built {
    pub graph: Graph,
    pub targets: Vec<FuncId>,
    pub shapes: Vec<Option<ShapeKey>>,
    pub keep: Vec<ShapeKey>,
    pub codes: Vec<FuncId>,
}

#[derive(Clone, Copy)]
pub(crate) struct Direct {
    native: u64,
    index: u32,
    arity: u8,
    out: j8framework::FastOut,
    inline_push: bool,
}

pub(crate) fn directs_from(a: &j8framework::Agent<'_, '_>) -> Vec<Direct> {
    a.realm.fast_builtins.get().iter().map(|(index, e)| Direct { native: e.native as usize as u64, index, arity: e.call.arity(), out: e.out, inline_push: e.inline_push }).collect()
}

#[derive(Debug)]
pub enum Reject {
    Unsupported,
}

#[derive(Clone)]
struct VObj {
    proto: V,
    shape: Option<ShapeKey>,
    fields: Vec<V>,
}

#[derive(Clone)]
struct Mem {
    ctx: V,
    idx: u32,
    val: V,
    dirty: bool,
}

#[derive(Clone, Default)]
struct Heap {
    objs: Vec<(V, VObj)>,
    mem: Vec<Mem>,
    known: Vec<Fact>,
}

#[derive(Clone, PartialEq)]
enum Fact {
    Shape(V, usize),
    Load(u8, V, i32, V),
    Object(V, V),
    Func(V, FuncId),
    NoProto(V),
    Distinct(V, V),
    Env(V, V),
    Global(u32, u32, V),
}

impl Fact {
    fn map(&self, f: impl Fn(V) -> V) -> Fact {
        match self {
            Fact::Shape(v, s) => Fact::Shape(f(*v), *s),
            Fact::Load(k, b, o, v) => Fact::Load(*k, f(*b), *o, f(*v)),
            Fact::Object(w, p) => Fact::Object(f(*w), f(*p)),
            Fact::Func(v, id) => Fact::Func(f(*v), *id),
            Fact::NoProto(v) => Fact::NoProto(f(*v)),
            Fact::Distinct(a, b) => Fact::Distinct(f(*a), f(*b)),
            Fact::Env(w, c) => Fact::Env(f(*w), f(*c)),
            Fact::Global(func, k, v) => Fact::Global(*func, *k, f(*v)),
        }
    }
}

#[derive(Clone)]
struct Regs {
    params: Vec<V>,
    regs: Vec<V>,
    acc: V,
    ctx: V,
}

#[derive(Clone)]
struct St {
    r: Regs,
    heap: Heap,
}

struct Caller {
    func: FuncId,
    pc: u32,
    callee: V,
    env: V,
    argc: u32,
    r: Regs,
    construct: Option<(V, V)>,
    parent: Option<Rc<Caller>>,
}

#[derive(Clone, Copy, Debug)]
struct Region {
    start: usize,
    end: usize,
    peel: Option<usize>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
enum Key {
    Reg(usize),
    Ctx,
    Mem(usize),
}

#[derive(Default, Clone)]
struct Widen {
    word: BTreeMap<(u32, usize, usize, Key), Rep>,
    real: BTreeMap<(u32, usize, usize, Key), ()>,
    flush: BTreeMap<(u32, usize, usize), ()>,
    unknown: BTreeMap<(u32, usize, usize), ()>,
    bad: Vec<((u32, usize, usize), Fact)>,
}

struct Header {
    block: B,
    phis: Vec<(Key, V)>,
    objs: Vec<(V, Vec<V>)>,
    mem: Vec<(V, u32, V)>,
    known: Vec<Fact>,
    st: St,
}

struct Act<'e> {
    f: Function<'e>,
    id: FuncId,
    cfg: Rc<Cfg>,
    callee: V,
    env: V,
    argc: u32,
    construct: Option<(V, V)>,
    caller: Option<Rc<Caller>>,
    depth: usize,
    inst: u32,
    regions: Vec<Region>,
    edges: BTreeMap<(usize, u32), Vec<(B, St)>>,
    headers: BTreeMap<(usize, u32), Header>,
    rets: Vec<(B, V, St)>,
}

struct Restart;

pub struct Builder<'e> {
    exe: &'e Executor,
    hints: &'e Hints,
    g: Graph,
    targets: Vec<FuncId>,
    shapes: Vec<Option<ShapeKey>>,
    keep: Vec<ShapeKey>,
    codes: Vec<FuncId>,
    cfgs: BTreeMap<u32, Rc<Cfg>>,
    widen: Widen,
    instances: u32,
    cur: Option<B>,
    undef: V,
    restart: bool,
    kind: Kind,
    root: FuncId,
    paths: &'e dyn Fn(bool, usize) -> Option<j8framework::StorePath>,
    directs: &'e [Direct],
    log: bool,
}

type R<T> = Result<T, Reject>;

impl<'e> Builder<'e> {
    pub fn build(exe: &'e Executor, hints: &'e Hints, root: FuncId, kind: Kind, paths: &'e dyn Fn(bool, usize) -> Option<j8framework::StorePath>, directs: &'e [Direct], log: bool) -> R<Built> {
        let mut widen = Widen::default();
        for round in 0..RESTARTS {
            if log && round > 0 {
                std::eprintln!("j8jit: restart {round}");
            }
            let mut b = Builder { exe, hints, g: Graph::default(), targets: Vec::new(), shapes: Vec::new(), keep: Vec::new(), codes: Vec::new(), cfgs: BTreeMap::new(), widen: widen.clone(), instances: 0, cur: None, undef: 0, restart: false, kind, root, paths, directs, log };
            if let Err(e) = b.run() {
                if log {
                    std::eprintln!("j8jit: rejected {e:?}");
                }
                return Err(e);
            }
            if !b.restart {
                crate::opt::simplify(&mut b.g);
                return Ok(Built { graph: b.g, targets: b.targets, shapes: b.shapes, keep: b.keep, codes: b.codes });
            }
            widen = b.widen;
        }
        Err(Reject::Unsupported)
    }

    fn cfg(&mut self, f: Function<'_>) -> Rc<Cfg> {
        self.cfgs.entry(f.id().0).or_insert_with(|| Rc::new(Cfg::new(f))).clone()
    }

    fn hint(&self, f: FuncId, pc: usize) -> u8 {
        self.hints.get(&(f.0, u32::try_from(pc).unwrap_or(u32::MAX))).copied().unwrap_or(0)
    }

    fn emit(&mut self, op: Op, rep: Rep) -> V {
        let b = self.cur.expect("emitting into a live block");
        self.g.push(b, op, rep)
    }

    fn word(&mut self, w: u64) -> V {
        self.emit(Op::Word(w), Rep::Word)
    }

    fn int(&mut self, i: i32) -> V {
        self.emit(Op::I32(i), Rep::I32)
    }

    fn const_int(&self, v: V) -> Option<i32> {
        match self.g.op(v) {
            Op::I32(i) => Some(*i),
            _ => None,
        }
    }

    fn const_word(&self, v: V) -> Option<u64> {
        match self.g.op(v) {
            Op::Word(w) => Some(*w),
            _ => None,
        }
    }

    fn emit_before_end(&mut self, b: B, op: Op, rep: Rep) -> V {
        let v = u32::try_from(self.g.insts.len()).unwrap_or(u32::MAX);
        self.g.insts.push(crate::ir::Inst { op, rep, block: b });
        let blk = &mut self.g.blocks[b as usize];
        let at = blk.insts.len().saturating_sub(1);
        blk.insts.insert(at, v);
        v
    }

    fn run(&mut self) -> R<()> {
        let entry = self.g.add_block();
        self.cur = Some(entry);
        self.undef = self.word(ValueBits::UNDEFINED);
        let f = self.exe.jit_function(self.root);
        let cfg = self.cfg(f);
        if !cfg.ok {
            return Err(Reject::Unsupported);
        }
        let (p, r) = (cfg.params, cfg.regs);
        self.g.entry_params = p;
        self.g.entry_regs = r;
        let mut regs = Regs { params: Vec::new(), regs: Vec::new(), acc: self.undef, ctx: self.undef };
        let callee;
        let argc;
        let mut construct = None;
        let regions;
        let start_pc;
        match self.kind {
            Kind::Enter | Kind::Osr(_) => {
                let at = |i: usize| u32::try_from(i).unwrap_or(0);
                for i in 0..p {
                    regs.params.push(self.emit(Op::Param(at(i)), Rep::Word));
                }
                for i in 0..r {
                    regs.regs.push(self.emit(Op::Param(at(p + i)), Rep::Word));
                }
                regs.ctx = self.emit(Op::Param(at(p + r)), Rep::Word);
                callee = self.emit(Op::Param(at(p + r + 1)), Rep::Word);
                regs.acc = self.emit(Op::Param(at(p + r + 2)), Rep::Word);
                let nt = self.emit(Op::Param(at(p + r + 3)), Rep::Word);
                let this = self.emit(Op::Param(at(p + r + 4)), Rep::Word);
                argc = u32::MAX;
                if f.constructs() || f.class_constructor() {
                    construct = Some((nt, this));
                }
                match self.kind {
                    Kind::Osr(j) => {
                        let (h, _) = cfg.loop_at(j).ok_or(Reject::Unsupported)?;
                        regions = Self::osr_regions(&cfg, h, f.len().saturating_sub(1));
                        start_pc = h;
                    }
                    _ => {
                        regions = vec![Region { start: 0, end: f.len().saturating_sub(1), peel: None }];
                        start_pc = 0;
                    }
                }
            }
            Kind::Native(n) => {
                callee = self.emit(Op::Callee, Rep::Word);
                for i in 0..p {
                    let v = if i <= n { self.emit(Op::Param(u32::try_from(i).unwrap_or(0)), Rep::Word) } else { self.undef };
                    regs.params.push(v);
                }
                regs.regs = vec![self.undef; r];
                let cp = self.emit(Op::Untag(callee), Rep::Ptr);
                regs.ctx = self.closure_env(cp);
                argc = u32::try_from(n).unwrap_or(0);
                regions = vec![Region { start: 0, end: f.len().saturating_sub(1), peel: None }];
                start_pc = 0;
            }
        }
        let st = St { r: regs, heap: Heap::default() };
        let mut act = self.activation(f, callee, argc, construct, None, 0, regions);
        if let Kind::Native(_) = self.kind {
            act.env = st.r.ctx;
        }
        if let Kind::Native(_) = self.kind {
            let fs = self.state(&act, &st, 0);
            self.emit(Op::Enter(fs), Rep::None);
        }
        let from = self.jump(u32::MAX);
        let key = (0, cfg.block_of[start_pc]);
        act.edges.entry(key).or_default().push((from, st));
        let _ = self.walk(&mut act);
        Ok(())
    }

    fn jump(&mut self, b: B) -> B {
        let from = self.cur.expect("a live block");
        self.g.push(from, Op::Jump(b), Rep::None);
        self.cur = None;
        from
    }

    fn osr_regions(cfg: &Cfg, h: usize, last: usize) -> Vec<Region> {
        let loops = cfg.enclosing(h);
        let mut out = Vec::new();
        let mut prev_end = None;
        for (lh, lj) in loops {
            match prev_end {
                None => out.push(Region { start: lh, end: lj, peel: Some(lh) }),
                Some(e) => out.push(Region { start: e + 1, end: lj, peel: Some(lh) }),
            }
            out.push(Region { start: lh, end: lj, peel: None });
            prev_end = Some(lj);
        }
        if let Some(e) = prev_end
            && e < last
        {
            out.push(Region { start: e + 1, end: last, peel: None });
        }
        out
    }

    fn activation(&mut self, f: Function<'e>, callee: V, argc: u32, construct: Option<(V, V)>, caller: Option<Rc<Caller>>, depth: usize, regions: Vec<Region>) -> Act<'e> {
        let cfg = self.cfg(f);
        self.instances += 1;
        Act { f, id: f.id(), cfg, callee, env: u32::MAX, argc, construct, caller, depth, inst: self.instances, regions, edges: BTreeMap::new(), headers: BTreeMap::new(), rets: Vec::new() }
    }

    fn walk(&mut self, act: &mut Act<'e>) -> Result<(), Restart> {
        for ri in 0..act.regions.len() {
            let reg = act.regions[ri];
            let first = act.cfg.block_of[reg.start];
            let last = act.cfg.block_of[reg.end];
            for bb in first..=last {
                let Some(edges) = act.edges.remove(&(ri, bb)) else { continue };
                let start = act.cfg.starts[bb as usize];
                let header = act.cfg.loops.iter().any(|&(h, j)| h == start && reg.start <= j && j <= reg.end && reg.peel != Some(h));
                let st = self.enter_block(act, ri, bb, edges, header);
                let Some(st) = st else { continue };
                self.block(act, ri, bb, st)?;
                if self.restart {
                    return Err(Restart);
                }
            }
        }
        Ok(())
    }

    fn enter_block(&mut self, act: &mut Act<'e>, ri: usize, bb: u32, edges: Vec<(B, St)>, header: bool) -> Option<St> {
        let b = self.g.add_block();
        for (p, _) in &edges {
            self.retarget(*p, b);
        }
        self.g.blocks[b as usize].preds = edges.iter().map(|(p, _)| *p).collect();
        let start = act.cfg.starts[bb as usize];
        let live = act.cfg.live[start].clone();
        let st = self.merge(act, ri, bb, b, &edges, &live, header);
        self.cur = Some(b);
        if header {
            self.g.blocks[b as usize].loop_header = true;
        }
        Some(st)
    }

    fn retarget(&mut self, p: B, b: B) {
        if let Some(&last) = self.g.blocks[p as usize].insts.last()
            && let Op::Jump(t) = &mut self.g.insts[last as usize].op
        {
            *t = b;
        }
    }

    fn get(cfg: &Cfg, r: &Regs, i: usize) -> V {
        if i < cfg.params {
            r.params[i]
        } else if i < cfg.params + cfg.regs {
            r.regs[i - cfg.params]
        } else {
            r.acc
        }
    }

    fn put(cfg: &Cfg, r: &mut Regs, i: usize, v: V) {
        if i < cfg.params {
            r.params[i] = v;
        } else if i < cfg.params + cfg.regs {
            r.regs[i - cfg.params] = v;
        } else {
            r.acc = v;
        }
    }

    fn join(a: Rep, b: Rep) -> Rep {
        match (a, b) {
            (x, y) if x == y => x,
            (Rep::I32, Rep::F64) | (Rep::F64, Rep::I32) => Rep::F64,
            _ => Rep::Word,
        }
    }

    fn convert_at(&mut self, p: B, v: V, rep: Rep) -> V {
        let have = self.g.rep(v);
        if have == rep {
            return v;
        }
        match (have, rep) {
            (Rep::I32, Rep::F64) => self.emit_before_end(p, Op::IntToFloat(v), Rep::F64),
            (Rep::I32, Rep::Word) => self.emit_before_end(p, Op::BoxInt(v), Rep::Word),
            (Rep::F64, Rep::Word) => self.emit_before_end(p, Op::BoxFloat(v), Rep::Word),
            (Rep::Bool, Rep::Word) => self.emit_before_end(p, Op::BoxBool(v), Rep::Word),
            (Rep::Ptr, Rep::Word) => self.emit_before_end(p, Op::BoxPtr(v, OBJECT_TAG), Rep::Word),
            (Rep::Ctx, Rep::Word) => self.emit_before_end(p, Op::BoxPtr(v, CONTEXT_TAG), Rep::Word),
            _ => unreachable!("join gives a representation every input converts to ({have:?} ⇒ {rep:?})"),
        }
    }

    fn materialize(&mut self, st: &mut St, id: V, p: Option<B>) -> V {
        let Some(pos) = st.heap.objs.iter().position(|(v, _)| *v == id) else { return id };
        let (_, o) = st.heap.objs.remove(pos);
        let mut fields = Vec::with_capacity(o.fields.len() + 1);
        fields.push(o.proto);
        for f in &o.fields {
            let r = self.materialize(st, *f, p);
            let w = match p {
                Some(p) => self.convert_at(p, r, Rep::Word),
                None => self.to_word(r),
            };
            fields.push(w);
        }
        let k = u32::try_from(self.shapes.len()).unwrap_or(0);
        self.shapes.push(o.shape);
        let op = Op::Call(Helper::Alloc(k), fields, None);
        let real = match p {
            Some(p) => self.emit_before_end(p, op, Rep::Ptr),
            None => self.emit(op, Rep::Ptr),
        };
        Self::replace(st, id, real);
        real
    }

    fn replace(st: &mut St, from: V, to: V) {
        let r = &mut st.r;
        for v in r.params.iter_mut().chain(r.regs.iter_mut()).chain([&mut r.acc, &mut r.ctx]) {
            if *v == from {
                *v = to;
            }
        }
        for (_, o) in &mut st.heap.objs {
            if o.proto == from {
                o.proto = to;
            }
            for f in &mut o.fields {
                if *f == from {
                    *f = to;
                }
            }
        }
        for m in &mut st.heap.mem {
            if m.val == from {
                m.val = to;
            }
            if m.ctx == from {
                m.ctx = to;
            }
        }
    }

    fn is_virtual(st: &St, v: V) -> bool {
        st.heap.objs.iter().any(|(id, _)| *id == v)
    }

    fn vobj(st: &St, v: V) -> Option<&VObj> {
        st.heap.objs.iter().find(|(id, _)| *id == v).map(|(_, o)| o)
    }

    #[expect(clippy::too_many_arguments, reason = "the parts of a merge")]
    fn merge(&mut self, act: &mut Act<'e>, ri: usize, bb: u32, b: B, edges: &[(B, St)], live: &Bits, header: bool) -> St {
        let cfg = act.cfg.clone();
        let n = cfg.params + cfg.regs + 1;
        let mut edges: Vec<(B, St)> = edges.to_vec();
        let hkey = |k: Key| (act.inst, ri, bb as usize, k);
        let slots: Vec<Key> = (0..n).filter(|&i| live.get(i)).map(Key::Reg).chain([Key::Ctx]).collect();
        let slot_val = |st: &St, k: Key| match k {
            Key::Reg(i) => Self::get(&cfg, &st.r, i),
            Key::Ctx => st.r.ctx,
            Key::Mem(_) => unreachable!("memory is merged below"),
        };
        loop {
            let mut changed = false;
            for &k in &slots {
                let vals: Vec<V> = edges.iter().map(|(_, s)| slot_val(s, k)).collect();
                let virt: Vec<bool> = edges.iter().zip(&vals).map(|((_, s), v)| Self::is_virtual(s, *v)).collect();
                let all_same = vals.iter().all(|v| *v == vals[0]);
                let any_virt = virt.iter().any(|x| *x);
                let force = header && self.widen.real.contains_key(&hkey(k));
                if any_virt && (!all_same || force || (edges.len() > 1 && !virt.iter().all(|x| *x))) {
                    for (j, (p, s)) in edges.iter_mut().enumerate() {
                        if virt[j] {
                            self.materialize(s, vals[j], Some(*p));
                            changed = true;
                        }
                    }
                }
            }
            if !changed {
                break;
            }
        }
        let mut out = edges[0].1.clone();
        out.r.params.iter_mut().chain(out.r.regs.iter_mut()).for_each(|v| *v = self.undef);
        out.r.acc = self.undef;
        let mut phis = Vec::new();
        for &k in &slots {
            let vals: Vec<V> = edges.iter().map(|(_, s)| slot_val(s, k)).collect();
            let merged = if !header && vals.iter().all(|v| *v == vals[0]) {
                vals[0]
            } else if header && Self::is_virtual(&edges[0].1, vals[0]) && edges.len() == 1 {
                vals[0]
            } else {
                let mut rep = vals.iter().map(|v| self.g.rep(*v)).reduce(Self::join).unwrap_or(Rep::Word);
                if header && let Some(w) = self.widen.word.get(&hkey(k)) {
                    rep = Self::join(rep, *w);
                }
                if rep == Rep::Bool {
                    rep = Rep::Word;
                }
                let inputs: Vec<V> = edges.iter().zip(&vals).map(|((p, _), v)| self.convert_at(*p, *v, rep)).collect();
                let phi = self.g.push_phi(b, inputs, rep);
                phis.push((k, phi));
                phi
            };
            match k {
                Key::Reg(i) => Self::put(&cfg, &mut out.r, i, merged),
                Key::Ctx => out.r.ctx = merged,
                Key::Mem(_) => {}
            }
        }
        let ids: Vec<V> = out.heap.objs.iter().map(|(id, _)| *id).filter(|id| Self::reachable(&out, *id)).collect();
        out.heap.objs.retain(|(id, _)| ids.contains(id));
        let mut hobjs = Vec::new();
        for id in ids {
            let Some(first) = Self::vobj(&edges[0].1, id).cloned() else { continue };
            let mut fields = Vec::new();
            for (fi, f0) in first.fields.iter().enumerate() {
                let vals: Vec<V> = edges.iter().map(|(_, s)| Self::vobj(s, id).and_then(|o| o.fields.get(fi).copied()).unwrap_or(*f0)).collect();
                if !header && vals.iter().all(|v| *v == vals[0]) {
                    fields.push(vals[0]);
                    continue;
                }
                let rep = vals.iter().map(|v| self.g.rep(*v)).reduce(Self::join).unwrap_or(Rep::Word);
                let rep = if rep == Rep::Bool { Rep::Word } else { rep };
                let inputs: Vec<V> = edges.iter().zip(&vals).map(|((p, _), v)| self.convert_at(*p, *v, rep)).collect();
                fields.push(self.g.push_phi(b, inputs, rep));
            }
            let protos: Vec<V> = edges.iter().map(|(_, s)| Self::vobj(s, id).map_or(first.proto, |o| o.proto)).collect();
            let proto = if !header && protos.iter().all(|v| *v == protos[0]) { protos[0] } else { self.g.push_phi(b, protos, Rep::Ptr) };
            if header {
                hobjs.push((id, core::iter::once(proto).chain(fields.iter().copied()).collect()));
            }
            if let Some((_, o)) = out.heap.objs.iter_mut().find(|(v, _)| *v == id) {
                o.fields = fields;
                o.proto = proto;
            }
        }
        let flush = header && self.widen.flush.contains_key(&(act.inst, ri, bb as usize));
        let mut mem = Vec::new();
        let mut hmem = Vec::new();
        let remap: Vec<(V, V)> = phis.iter().filter_map(|(k, phi)| matches!(k, Key::Ctx | Key::Reg(_)).then(|| (slot_val(&edges[0].1, *k), *phi))).collect();
        let rekey = |c: V| remap.iter().find(|(from, _)| *from == c).map_or(c, |(_, to)| *to);
        if !flush {
            for (mi, m) in edges[0].1.heap.mem.clone().iter().enumerate() {
                let vals: Option<Vec<(V, bool)>> = edges.iter().map(|(_, s)| s.heap.mem.iter().find(|x| x.ctx == m.ctx && x.idx == m.idx).map(|x| (x.val, x.dirty))).collect();
                let Some(vals) = vals else { continue };
                let dirty = vals.iter().any(|(_, d)| *d);
                let keep_virtual = header && edges.len() == 1 && Self::is_virtual(&edges[0].1, vals[0].0) && !self.widen.real.contains_key(&hkey(Key::Mem(mi)));
                let val = if keep_virtual || (!header && vals.iter().all(|(v, _)| *v == vals[0].0)) {
                    vals[0].0
                } else {
                    let rep = vals.iter().map(|(v, _)| self.g.rep(*v)).reduce(Self::join).unwrap_or(Rep::Word);
                    let mut rep = if rep == Rep::Bool { Rep::Word } else { rep };
                    if header && let Some(w) = self.widen.word.get(&hkey(Key::Mem(mem.len()))) {
                        rep = Self::join(rep, *w);
                    }
                    if vals.iter().any(|(v, _)| edges.iter().any(|(_, s)| Self::is_virtual(s, *v))) {
                        for (p, s) in &mut edges {
                            let x = s.heap.mem.iter().find(|x| x.ctx == m.ctx && x.idx == m.idx).map(|x| x.val);
                            if let Some(x) = x
                                && Self::is_virtual(s, x)
                            {
                                self.materialize(s, x, Some(*p));
                            }
                        }
                        rep = Rep::Word;
                    }
                    let inputs: Vec<V> = edges.iter().map(|(p, s)| {
                        let x = s.heap.mem.iter().find(|x| x.ctx == m.ctx && x.idx == m.idx).map_or(self.undef, |x| x.val);
                        (*p, x)
                    }).collect::<Vec<_>>().into_iter().map(|(p, x)| self.convert_at(p, x, rep)).collect();
                    let phi = self.g.push_phi(b, inputs, rep);
                    if header {
                        hmem.push((rekey(m.ctx), m.idx, phi));
                    }
                    phi
                };
                mem.push(Mem { ctx: rekey(m.ctx), idx: m.idx, val, dirty });
            }
        }
        for (p, s) in &mut edges {
            for m in s.heap.mem.clone() {
                if m.dirty && !mem.iter().any(|x| x.ctx == m.ctx && x.idx == m.idx) {
                    self.write_back(s, &m, Some(*p));
                }
            }
        }
        out.heap.mem = mem;
        let mut known: Vec<Fact> = edges[0].1.heap.known.clone();
        known.retain(|f| edges.iter().all(|(_, s)| s.heap.known.contains(f)));
        if header && self.widen.unknown.contains_key(&(act.inst, ri, bb as usize)) {
            known.clear();
        }
        if header {
            let hk = (act.inst, ri, bb as usize);
            let mut into: Vec<(V, V)> = Vec::new();
            for &i in &self.g.blocks[b as usize].insts.clone() {
                if let Op::Phi(ins) = self.g.op(i)
                    && let Some(&x) = ins.first()
                {
                    into.push((x, i));
                }
            }
            let sub = |v: V| into.iter().find(|(x, _)| *x == v).map_or(v, |(_, phi)| *phi);
            let extra: Vec<Fact> = known.iter().filter_map(|f| {
                let g = match f {
                    Fact::Load(k, b, o, v) => Fact::Load(*k, sub(*b), *o, *v),
                    Fact::Object(w, p) => Fact::Object(sub(*w), *p),
                    Fact::Env(w, c) => Fact::Env(sub(*w), *c),
                    f => f.map(sub),
                };
                (g != *f).then_some(g)
            }).collect();
            known.extend(extra);
            known.retain(|f| !self.widen.bad.iter().any(|(k, b)| *k == hk && b == f));
        }
        out.heap.known = known.clone();
        if header {
            if edges.len() == 1 {
                let pc = act.cfg.starts[bb as usize];
                let fs = self.state(act, &edges[0].1, pc);
                self.g.preheaders.push((b, edges[0].0, fs));
            }
            act.headers.insert((ri, bb), Header { block: b, phis, objs: hobjs, mem: hmem, known, st: out.clone() });
        }
        out
    }

    fn reachable(st: &St, id: V) -> bool {
        let r = &st.r;
        r.params.iter().chain(&r.regs).chain([&r.acc, &r.ctx]).any(|v| *v == id) || st.heap.objs.iter().any(|(o, x)| *o != id && (x.fields.contains(&id) || x.proto == id)) || st.heap.mem.iter().any(|m| m.val == id)
    }

    fn write_back(&mut self, st: &mut St, m: &Mem, p: Option<B>) {
        let val = self.materialize(st, m.val, p);
        let (cell, w) = match p {
            Some(p) => {
                let w = self.convert_at(p, val, Rep::Word);
                (self.emit_before_end(p, Op::ContextCell(m.ctx, m.idx), Rep::Ptr), w)
            }
            None => {
                let w = self.to_word(val);
                (self.emit(Op::ContextCell(m.ctx, m.idx), Rep::Ptr), w)
            }
        };
        let op = Op::StoreCell(cell, 0, w);
        match p {
            Some(p) => self.emit_before_end(p, op, Rep::None),
            None => self.emit(op, Rep::None),
        };
        if let Some(x) = st.heap.mem.iter_mut().find(|x| x.ctx == m.ctx && x.idx == m.idx) {
            x.dirty = false;
        }
    }

    fn flush(&mut self, st: &mut St) {
        for m in st.heap.mem.clone() {
            if m.dirty {
                self.write_back(st, &m, None);
            }
        }
    }

    fn forget(st: &mut St) {
        st.heap.mem.retain(|m| m.dirty);
        st.heap.known.clear();
    }

    fn stored(st: &mut St) {
        st.heap.known.retain(|f| !matches!(f, Fact::Load(LOAD_WORD, ..)));
    }

    fn shape_check(&mut self, act: &Act<'e>, st: &mut St, p: V, shape: Option<ShapeKey>, pc: usize) {
        let addr = shape.as_ref().map_or(0, |s| s.id());
        if st.heap.known.contains(&Fact::Shape(p, addr)) {
            return;
        }
        self.keep(&shape);
        let fs = self.state(act, st, pc);
        self.emit(Op::CheckShape(p, shape, fs), Rep::None);
        st.heap.known.push(Fact::Shape(p, addr));
    }

    fn load(&mut self, st: &mut St, kind: u8, base: V, off: i32) -> V {
        if let Some(Fact::Load(.., v)) = st.heap.known.iter().find(|f| matches!(f, Fact::Load(k, b, o, _) if *k == kind && *b == base && *o == off)) {
            return *v;
        }
        let (op, rep) = match kind {
            LOAD_WORD => (Op::LoadWord(base, off), Rep::Word),
            LOAD_PTR => (Op::LoadPtr(base, off), Rep::Ptr),
            _ => (Op::LoadRaw(base, off), Rep::Ctx),
        };
        let v = self.emit(op, rep);
        st.heap.known.push(Fact::Load(kind, base, off, v));
        v
    }

    fn to_word(&mut self, v: V) -> V {
        match self.g.rep(v) {
            Rep::Word => v,
            Rep::I32 => self.emit(Op::BoxInt(v), Rep::Word),
            Rep::F64 => self.emit(Op::BoxFloat(v), Rep::Word),
            Rep::Bool => self.emit(Op::BoxBool(v), Rep::Word),
            Rep::Ptr => self.emit(Op::BoxPtr(v, OBJECT_TAG), Rep::Word),
            Rep::Ctx => self.emit(Op::BoxPtr(v, CONTEXT_TAG), Rep::Word),
            Rep::None => self.undef,
        }
    }

    fn real(&mut self, st: &mut St, v: V) -> V {
        let v = if Self::is_virtual(st, v) { self.materialize(st, v, None) } else { v };
        self.to_word(v)
    }

    fn state(&mut self, act: &Act<'e>, st: &St, pc: usize) -> Fs {
        let mut objects: Vec<VirtualObject> = Vec::new();
        let mut ids: Vec<V> = Vec::new();
        let mut frames = Vec::new();
        let mut chain = Vec::new();
        let mut c = act.caller.clone();
        while let Some(x) = c {
            c = x.parent.clone();
            chain.push(x);
        }
        chain.reverse();
        for c in &chain {
            frames.push(self.frame(st, &mut objects, &mut ids, c.func, c.pc, c.callee, c.argc, &c.r, c.construct, true, c.env));
        }
        frames.push(self.frame(st, &mut objects, &mut ids, act.id, u32::try_from(pc).unwrap_or(0), act.callee, act.argc, &st.r, act.construct, false, act.env));
        let mut writes = Vec::new();
        for m in &st.heap.mem {
            if m.dirty {
                let s = self.slot(st, &mut objects, &mut ids, m.val);
                writes.push((m.ctx, m.idx, s));
            }
        }
        self.g.add_state(FrameState { frames, objects, writes })
    }

    #[expect(clippy::too_many_arguments, reason = "a frame's parts")]
    fn frame(&mut self, st: &St, objects: &mut Vec<VirtualObject>, ids: &mut Vec<V>, func: FuncId, pc: u32, callee: V, argc: u32, r: &Regs, construct: Option<(V, V)>, caller: bool, env: V) -> Frame {
        let callee = self.slot(st, objects, ids, callee);
        let params = r.params.iter().map(|v| self.slot(st, objects, ids, *v)).collect();
        let regs = r.regs.iter().map(|v| self.slot(st, objects, ids, *v)).collect();
        let context = if r.ctx == env { Slot::Closure } else { self.slot(st, objects, ids, r.ctx) };
        let acc = if caller { Slot::Value(self.undef) } else { self.slot(st, objects, ids, r.acc) };
        let construct = construct.map(|(n, t)| (self.slot(st, objects, ids, n), self.slot(st, objects, ids, t)));
        Frame { func, pc, callee, params, argc, regs, context, acc, construct }
    }

    fn slot(&mut self, st: &St, objects: &mut Vec<VirtualObject>, ids: &mut Vec<V>, v: V) -> Slot {
        let Some(o) = Self::vobj(st, v).cloned() else { return Slot::Value(v) };
        if let Some(i) = ids.iter().position(|x| *x == v) {
            return Slot::Virtual(u32::try_from(i).unwrap_or(0));
        }
        let i = objects.len();
        ids.push(v);
        objects.push(VirtualObject { proto: o.proto, shape: o.shape, fields: Vec::new() });
        let fields = o.fields.iter().map(|f| self.slot(st, objects, ids, *f)).collect();
        objects[i].fields = fields;
        Slot::Virtual(u32::try_from(i).unwrap_or(0))
    }

    fn block(&mut self, act: &mut Act<'e>, ri: usize, bb: u32, mut st: St) -> Result<(), Restart> {
        let cfg = act.cfg.clone();
        let reg = act.regions[ri];
        let start = cfg.starts[bb as usize];
        let end = cfg.starts.get(bb as usize + 1).copied().unwrap_or(act.f.len());
        for pc in start..end {
            if self.cur.is_none() {
                return Ok(());
            }
            let d = act.f.insn(pc);
            match self.insn(act, &mut st, pc, d) {
                Ok(Flow::Next) => {}
                Ok(Flow::Jump(t)) => {
                    self.edge(act, ri, &reg, pc, t, st, false);
                    return Ok(());
                }
                Ok(Flow::Branch(cond, t, on_true)) => {
                    let b = self.cur.expect("a live block");
                    let (bt, bf) = (self.g.add_block(), self.g.add_block());
                    let op = if on_true { Op::Branch(cond, bt, bf) } else { Op::Branch(cond, bf, bt) };
                    self.g.push(b, op, Rep::None);
                    for side in [bt, bf] {
                        self.g.blocks[side as usize].preds.push(b);
                        self.g.push(side, Op::Jump(u32::MAX), Rep::None);
                    }
                    self.cur = None;
                    self.edge_from(act, ri, &reg, bt, t, st.clone(), false);
                    self.edge_from(act, ri, &reg, bf, pc + 1, st, false);
                    return Ok(());
                }
                Ok(Flow::Loop(t)) => {
                    self.edge(act, ri, &reg, pc, t, st, true);
                    return Ok(());
                }
                Ok(Flow::End) => return Ok(()),
                Err(Reject::Unsupported) => {
                    let fs = self.state(act, &st, pc);
                    self.emit(Op::Deopt(fs), Rep::None);
                    self.cur = None;
                    return Ok(());
                }
            }
            if self.restart {
                return Err(Restart);
            }
        }
        if self.cur.is_some() {
            self.edge(act, ri, &reg, end - 1, end, st, false);
        }
        Ok(())
    }

    #[expect(clippy::too_many_arguments, reason = "an edge's parts")]
    fn edge(&mut self, act: &mut Act<'e>, ri: usize, reg: &Region, _pc: usize, t: usize, st: St, back: bool) {
        let b = self.cur.expect("a live block");
        self.g.push(b, Op::Jump(u32::MAX), Rep::None);
        self.cur = None;
        self.edge_from(act, ri, reg, b, t, st, back);
    }

    #[expect(clippy::too_many_arguments, reason = "an edge's parts")]
    fn edge_from(&mut self, act: &mut Act<'e>, ri: usize, reg: &Region, b: B, t: usize, st: St, back: bool) {
        let tb = act.cfg.block_of.get(t).copied().unwrap_or(u32::MAX);
        let target = if back && reg.peel == Some(t) {
            ri + 1
        } else if t >= reg.start && t <= reg.end && !(back && reg.peel.is_some() && reg.peel != Some(t) && t < reg.start) {
            ri
        } else {
            match (ri + 1..act.regions.len()).find(|&r| act.regions[r].start <= t && t <= act.regions[r].end) {
                Some(r) => r,
                None => {
                    self.cur = Some(b);
                    self.undo_jump(b);
                    let fs = self.state(act, &st, t);
                    self.emit_exit(b, fs);
                    return;
                }
            }
        };
        if back && target == ri {
            self.back_edge(act, ri, tb, b, st);
            return;
        }
        act.edges.entry((target, tb)).or_default().push((b, st));
    }

    fn undo_jump(&mut self, b: B) {
        if let Some(&last) = self.g.blocks[b as usize].insts.last()
            && matches!(self.g.insts[last as usize].op, Op::Jump(_))
        {
            self.g.blocks[b as usize].insts.pop();
        }
    }

    fn emit_exit(&mut self, _b: B, fs: Fs) {
        self.emit(Op::Deopt(fs), Rep::None);
        self.cur = None;
    }

    fn back_edge(&mut self, act: &mut Act<'e>, ri: usize, tb: u32, b: B, mut st: St) {
        let Some(h) = act.headers.get(&(ri, tb)) else {
            if self.log { std::eprintln!("j8jit: restart: no header"); }
            self.restart = true;
            return;
        };
        let cfg = act.cfg.clone();
        let hb = h.block;
        let key = |k: Key| (act.inst, ri, tb as usize, k);
        let mut fail = false;
        let mut map: Vec<(V, V)> = Vec::new();
        let slots: Vec<(Key, V)> = h.phis.clone();
        let hst = h.st.clone();
        let live = cfg.live[cfg.starts[tb as usize]].clone();
        let n = cfg.params + cfg.regs + 1;
        let keys: Vec<Key> = (0..n).filter(|&i| live.get(i)).map(Key::Reg).chain([Key::Ctx]).collect();
        let get = |s: &St, k: Key| match k {
            Key::Reg(i) => Self::get(&cfg, &s.r, i),
            Key::Ctx => s.r.ctx,
            Key::Mem(_) => 0,
        };
        for &k in &keys {
            let hv = get(&hst, k);
            let bv = get(&st, k);
            let hvirt = Self::vobj(&hst, hv).cloned();
            match hvirt {
                Some(ho) => match Self::vobj(&st, bv) {
                    Some(bo) if Self::same_shape(&ho.shape, &bo.shape) && ho.fields.len() == bo.fields.len() => match map.iter().find(|(x, _)| *x == hv) {
                        Some((_, y)) if *y != bv => {
                            self.widen.real.insert(key(k), ());
                            fail = true;
                        }
                        Some(_) => {}
                        None => map.push((hv, bv)),
                    },
                    _ => {
                        self.widen.real.insert(key(k), ());
                        fail = true;
                    }
                },
                None => {
                    if Self::is_virtual(&st, bv) {
                    }
                }
            }
        }
        for (mi, hm) in hst.heap.mem.iter().enumerate() {
            let Some(ho) = Self::vobj(&hst, hm.val).cloned() else { continue };
            let bv = st.heap.mem.iter().find(|m| m.ctx == hm.ctx && m.idx == hm.idx).map(|m| m.val);
            match bv.and_then(|bv| Self::vobj(&st, bv).map(|bo| (bv, bo.clone()))) {
                Some((bv, bo)) if Self::same_shape(&ho.shape, &bo.shape) && ho.fields.len() == bo.fields.len() => match map.iter().find(|(x, _)| *x == hm.val) {
                    Some((_, y)) if *y != bv => {
                        self.widen.real.insert(key(Key::Mem(mi)), ());
                        fail = true;
                    }
                    Some(_) => {}
                    None => map.push((hm.val, bv)),
                },
                _ => {
                    self.widen.real.insert(key(Key::Mem(mi)), ());
                    fail = true;
                }
            }
        }
        let h = act.headers.get(&(ri, tb)).expect("checked above");
        let back = |v: V| -> V {
            let phi_in = |p: V| -> Option<V> {
                if self.g.insts[p as usize].block != h.block {
                    return None;
                }
                if !matches!(self.g.op(p), Op::Phi(_)) {
                    return None;
                }
                h.phis.iter().find(|(_, x)| *x == p).map(|(k, _)| match k {
                    Key::Reg(i) => Self::get(&cfg, &st.r, *i),
                    Key::Ctx => st.r.ctx,
                    Key::Mem(_) => p,
                }).or_else(|| h.mem.iter().find(|(_, _, x)| *x == p).and_then(|(c, i, _)| st.heap.mem.iter().find(|m| m.ctx == *c && m.idx == *i).map(|m| m.val)))
                .or_else(|| {
                    let (hid, phis) = h.objs.iter().find(|(_, ph)| ph.contains(&p))?;
                    let j = phis.iter().position(|x| *x == p)?;
                    let (_, bid) = map.iter().find(|(x, _)| x == hid)?;
                    let bo = Self::vobj(&st, *bid)?;
                    Some(if j == 0 { bo.proto } else { *bo.fields.get(j - 1)? })
                })
            };
            phi_in(v).unwrap_or(v)
        };
        let holds = |f: &Fact| -> bool {
            let g = f.map(back);
            match g {
                Fact::Object(w, p) if w == p => true,
                g => st.heap.known.contains(&g),
            }
        };
        let broken: Vec<Fact> = h.known.iter().filter(|f| !holds(f)).cloned().collect();
        if !broken.is_empty() {
            for f in broken {
                self.widen.bad.push(((act.inst, ri, tb as usize), f));
            }
            if self.log {
                for f in &h.known {
                    if !st.heap.known.contains(&f.map(back)) {
                        let d = |f: &Fact| match f {
                            Fact::Shape(v, _) => alloc::format!("Shape(v{v})"),
                            Fact::Load(k, b, o, v) => alloc::format!("Load{k}(v{b}+{o} = v{v})"),
                            Fact::Object(w, p) => alloc::format!("Object(v{w} = v{p})"),
                            Fact::Func(v, _) => alloc::format!("Func(v{v})"),
                            Fact::NoProto(v) => alloc::format!("NoProto(v{v})"),
                            Fact::Distinct(a, b) => alloc::format!("Distinct(v{a}, v{b})"),
                            Fact::Env(w, c) => alloc::format!("Env(v{w} = v{c})"),
                            Fact::Global(_, k, v) => alloc::format!("Global({k} = v{v})"),
                        };
                        std::eprintln!("j8jit: restart: fact {} (back {})", d(f), d(&f.map(back)));
                    }
                }
            }
            fail = true;
        }
        let h = act.headers.get(&(ri, tb)).expect("checked above");
        for (ctx, idx, _) in &h.mem {
            if !st.heap.mem.iter().any(|m| m.ctx == *ctx && m.idx == *idx) {
                self.widen.flush.insert((act.inst, ri, tb as usize), ());
                fail = true;
            }
        }
        if fail {
            if self.log { std::eprintln!("j8jit: restart: virtual/mem mismatch"); }
            self.restart = true;
            return;
        }
        let mut inputs: Vec<(V, V)> = Vec::new();
        for (k, phi) in &slots {
            let mut v = get(&st, *k);
            if Self::is_virtual(&st, v) && !map.iter().any(|(_, y)| *y == v) {
                v = self.materialize(&mut st, v, Some(b));
            }
            if self.g.rep(*phi) == Rep::Word && v != *phi {
                let own = match self.g.op(v) {
                    Op::Untag(w) | Op::CheckObject(w, _) => *w == *phi,
                    _ => st.heap.known.contains(&Fact::Object(*phi, v)),
                };
                if own {
                    v = *phi;
                }
            }
            inputs.push((*phi, v));
        }
        let poll_fs = self.g.blocks[b as usize].insts.iter().rev().find_map(|&i| match self.g.op(i) {
            Op::Poll(fs) => Some(*fs),
            _ => None,
        });
        let h = act.headers.get(&(ri, tb)).expect("checked above");
        let hobjs = h.objs.clone();
        for (hid, phis) in &hobjs {
            let Some((_, bid)) = map.iter().find(|(x, _)| x == hid) else { continue };
            let Some(bo) = Self::vobj(&st, *bid).cloned() else { continue };
            for (j, phi) in phis.iter().enumerate() {
                let mut v = if j == 0 { bo.proto } else { bo.fields[j - 1] };
                if j == 0
                    && v != *phi
                    && let Some(fs) = poll_fs
                {
                    let eq = self.emit_before_end(b, Op::CmpWord(Cond::Eq, v, *phi), Rep::Bool);
                    self.emit_before_end(b, Op::Check(eq, true, fs), Rep::None);
                    v = *phi;
                }
                inputs.push((*phi, v));
            }
        }
        for (ctx, idx, phi) in &h.mem {
            if let Some(m) = st.heap.mem.iter().find(|m| m.ctx == *ctx && m.idx == *idx) {
                inputs.push((*phi, m.val));
            }
        }
        let hm = h.mem.clone();
        let tracked = h.st.heap.mem.clone();
        for m in st.heap.mem.clone() {
            if m.dirty && !tracked.iter().any(|x| x.ctx == m.ctx && x.idx == m.idx) {
                self.write_back(&mut st, &m, Some(b));
            }
        }
        for (phi, v) in &inputs {
            let want = self.g.rep(*phi);
            let have = self.g.rep(*v);
            if Self::is_virtual(&st, *v) {
                continue;
            }
            if Self::join(want, have) != want {
                if self.log {
                    std::eprintln!("j8jit: phi {phi} wants {want:?} has {have:?}");
                }
                let k = slots.iter().find(|(_, p)| p == phi).map(|(k, _)| *k).or_else(|| hm.iter().position(|(_, _, p)| p == phi).map(Key::Mem));
                if let Some(k) = k {
                    self.widen.word.insert(key(k), Self::join(want, have));
                    if self.log { std::eprintln!("j8jit: restart: rep widen"); }
                    self.restart = true;
                    return;
                }
                for (k, hv) in keys.iter().map(|k| (*k, get(&hst, *k))) {
                    if Self::is_virtual(&hst, hv) {
                        self.widen.real.insert(key(k), ());
                    }
                }
                if self.log { std::eprintln!("j8jit: restart: field rep"); }
                self.restart = true;
                return;
            }
        }
        self.retarget(b, hb);
        self.g.blocks[hb as usize].preds.push(b);
        for (phi, v) in inputs {
            let want = self.g.rep(phi);
            let v = self.convert_at(b, v, want);
            if let Op::Phi(ins) = &mut self.g.insts[phi as usize].op {
                ins.push(v);
            }
        }
    }

    fn same_shape(a: &Option<ShapeKey>, b: &Option<ShapeKey>) -> bool {
        match (a, b) {
            (None, None) => true,
            (Some(x), Some(y)) => x == y,
            _ => false,
        }
    }

    fn reg(act: &Act<'e>, st: &St, k: OperandValue) -> R<V> {
        match act.f.reg(k) {
            Reg::Param(i) => st.r.params.get(i).copied().ok_or(Reject::Unsupported),
            Reg::Local(i) => st.r.regs.get(i).copied().ok_or(Reject::Unsupported),
            Reg::Context => Ok(st.r.ctx),
            Reg::Closure => Ok(act.callee),
            Reg::Other => Err(Reject::Unsupported),
        }
    }

    fn set_reg(act: &Act<'e>, st: &mut St, k: OperandValue, v: V) -> R<()> {
        match act.f.reg(k) {
            Reg::Param(i) => *st.r.params.get_mut(i).ok_or(Reject::Unsupported)? = v,
            Reg::Local(i) => *st.r.regs.get_mut(i).ok_or(Reject::Unsupported)? = v,
            Reg::Context => st.r.ctx = v,
            _ => return Err(Reject::Unsupported),
        }
        Ok(())
    }

    fn as_int(&mut self, act: &Act<'e>, st: &mut St, v: V, pc: usize) -> R<Option<V>> {
        if self.hint(act.id, pc) & HINT_FLOAT != 0 {
            return Ok(None);
        }
        match self.g.rep(v) {
            Rep::I32 => Ok(Some(v)),
            Rep::F64 => {
                let fs = self.state(act, st, pc);
                let i = self.emit(Op::FloatToInt(v, fs), Rep::I32);
                Self::replace(st, v, i);
                Ok(Some(i))
            }
            Rep::Word => {
                if Self::is_virtual(st, v) {
                    return Err(Reject::Unsupported);
                }
                if let Some(w) = self.const_word(v) {
                    let x = f64::from_bits(w);
                    #[expect(clippy::cast_possible_truncation, reason = "checked by converting back")]
                    let i = x as i32;
                    if w < ValueBits::BOXED_MIN && f64::from(i) == x && !(i == 0 && x.is_sign_negative()) {
                        return Ok(Some(self.int(i)));
                    }
                    return Ok(None);
                }
                let fs = self.state(act, st, pc);
                let i = self.emit(Op::UnboxInt(v, fs), Rep::I32);
                Self::replace(st, v, i);
                Ok(Some(i))
            }
            _ => Ok(None),
        }
    }

    fn as_float(&mut self, act: &Act<'e>, st: &mut St, v: V, pc: usize) -> R<Option<V>> {
        if self.hint(act.id, pc) & HINT_GENERIC != 0 {
            return Ok(None);
        }
        match self.g.rep(v) {
            Rep::F64 => Ok(Some(v)),
            Rep::I32 => Ok(Some(self.emit(Op::IntToFloat(v), Rep::F64))),
            Rep::Word => {
                if Self::is_virtual(st, v) {
                    return Ok(None);
                }
                if let Some(w) = self.const_word(v) {
                    if w < ValueBits::BOXED_MIN {
                        return Ok(Some(self.emit(Op::F64(f64::from_bits(w)), Rep::F64)));
                    }
                    return Ok(None);
                }
                let fs = self.state(act, st, pc);
                let f = self.emit(Op::UnboxFloat(v, fs), Rep::F64);
                Self::replace(st, v, f);
                Ok(Some(f))
            }
            _ => Ok(None),
        }
    }

    fn numbers(&mut self, act: &Act<'e>, st: &mut St, l: V, r: V, pc: usize) -> R<Option<(V, V, bool)>> {
        let float_hint = self.hint(act.id, pc) & HINT_FLOAT != 0;
        let lr = self.g.rep(l);
        let rr = self.g.rep(r);
        if !float_hint && lr != Rep::F64 && rr != Rep::F64 {
            let a = self.as_int(act, st, l, pc)?;
            if let Some(a) = a {
                let r = if st.r.params.contains(&r) || st.r.regs.contains(&r) || st.r.acc == r { r } else { r };
                let b = self.as_int(act, st, r, pc)?;
                if let Some(b) = b {
                    return Ok(Some((a, b, true)));
                }
                let fa = self.as_float(act, st, a, pc)?;
                let fb = self.as_float(act, st, r, pc)?;
                return Ok(fa.zip(fb).map(|(x, y)| (x, y, false)));
            }
        }
        let fa = self.as_float(act, st, l, pc)?;
        let fb = self.as_float(act, st, r, pc)?;
        Ok(fa.zip(fb).map(|(x, y)| (x, y, false)))
    }

    fn arith(&mut self, act: &Act<'e>, st: &mut St, op: BinaryOp, l: V, r: V, pc: usize) -> R<V> {
        let int_op = match op {
            BinaryOp::Add => Some(IntOp::Add),
            BinaryOp::Sub => Some(IntOp::Sub),
            BinaryOp::Mul => Some(IntOp::Mul),
            BinaryOp::Rem => Some(IntOp::Mod),
            BinaryOp::BitAnd => Some(IntOp::And),
            BinaryOp::BitOr => Some(IntOp::Or),
            BinaryOp::BitXor => Some(IntOp::Xor),
            BinaryOp::Shl => Some(IntOp::Shl),
            BinaryOp::Shr => Some(IntOp::Sar),
            BinaryOp::UShr => Some(IntOp::Shr),
            _ => None,
        };
        let bitwise = matches!(op, BinaryOp::BitAnd | BinaryOp::BitOr | BinaryOp::BitXor | BinaryOp::Shl | BinaryOp::Shr | BinaryOp::UShr);
        if self.hint(act.id, pc) & HINT_GENERIC == 0 {
            if bitwise {
                if let (Some(a), Some(b)) = (self.as_int(act, st, l, pc)?, self.as_int(act, st, r, pc)?) {
                    let fs = (int_op == Some(IntOp::Shr)).then(|| self.state(act, st, pc));
                    return Ok(self.emit(Op::Int(int_op.unwrap_or(IntOp::And), a, b, fs), Rep::I32));
                }
            } else if let Some((a, b, ints)) = self.numbers(act, st, l, r, pc)? {
                if ints && let Some(iop) = int_op {
                    if let (Some(x), Some(y)) = (self.const_int(a), self.const_int(b))
                        && let Some(z) = Self::fold(iop, x, y)
                    {
                        return Ok(self.int(z));
                    }
                    let fs = self.state(act, st, pc);
                    return Ok(self.emit(Op::Int(iop, a, b, Some(fs)), Rep::I32));
                }
                let (fa, fb) = if ints { (self.emit(Op::IntToFloat(a), Rep::F64), self.emit(Op::IntToFloat(b), Rep::F64)) } else { (a, b) };
                return Ok(match op {
                    BinaryOp::Add => self.emit(Op::Float(FloatOp::Add, fa, fb), Rep::F64),
                    BinaryOp::Sub => self.emit(Op::Float(FloatOp::Sub, fa, fb), Rep::F64),
                    BinaryOp::Mul => self.emit(Op::Float(FloatOp::Mul, fa, fb), Rep::F64),
                    BinaryOp::Div => self.emit(Op::Float(FloatOp::Div, fa, fb), Rep::F64),
                    BinaryOp::Rem => self.emit(Op::Call(Helper::ModF, vec![fa, fb], None), Rep::F64),
                    _ => return self.generic_binary(act, st, op, l, r, pc),
                });
            }
        }
        self.generic_binary(act, st, op, l, r, pc)
    }

    fn fold(op: IntOp, x: i32, y: i32) -> Option<i32> {
        match op {
            IntOp::Add => x.checked_add(y),
            IntOp::Sub => x.checked_sub(y),
            IntOp::Mul => x.checked_mul(y).filter(|z| *z != 0 || (x >= 0 && y >= 0)),
            IntOp::And => Some(x & y),
            IntOp::Or => Some(x | y),
            IntOp::Xor => Some(x ^ y),
            _ => None,
        }
    }

    fn helper_call(&mut self, act: &Act<'e>, st: &mut St, h: Helper, args: &[V], pc: usize) -> V {
        let ws: Vec<V> = args.iter().map(|a| self.real(st, *a)).collect();
        self.flush(st);
        let fs = self.state(act, st, pc);
        let v = self.emit(Op::Call(h, ws, Some(fs)), h.rep());
        Self::forget(st);
        v
    }

    fn concat_values(&mut self, act: &Act<'e>, st: &mut St, l: V, r: V, pc: usize) -> V {
        let a = self.real(st, l);
        let b = self.real(st, r);
        self.flush(st);
        let fs = self.state(act, st, pc);
        let v = self.emit(Op::Call(Helper::Concat, vec![a, b], Some(fs)), Rep::Word);
        Self::forget(st);
        v
    }

    fn string_empty(&mut self) -> V {
        let raw = self.emit(Op::CtxField(Ctx::EMPTY), Rep::Ptr);
        self.emit(Op::BoxPtr(raw, STRING_TAG), Rep::Word)
    }

    fn string_byte(&mut self, byte: u8) -> V {
        let base = self.emit(Op::CtxField(Ctx::CHARS), Rep::Ptr);
        let raw = self.emit(Op::LoadRaw(base, i32::from(byte) * 8), Rep::Ptr);
        self.emit(Op::BoxPtr(raw, STRING_TAG), Rep::Word)
    }

    fn generic_binary(&mut self, act: &Act<'e>, st: &mut St, op: BinaryOp, l: V, r: V, pc: usize) -> R<V> {
        let a = self.real(st, l);
        let b = self.real(st, r);
        self.flush(st);
        let fs = self.state(act, st, pc);
        let v = self.emit(Op::Call(Helper::Binary(op), vec![a, b], Some(fs)), Rep::Word);
        Self::forget(st);
        Ok(v)
    }

    fn compare(&mut self, act: &Act<'e>, st: &mut St, op: CompareOp, l: V, r: V, pc: usize) -> R<V> {
        let cond = match op {
            CompareOp::Lt => Cond::Lt,
            CompareOp::Le => Cond::Le,
            CompareOp::Gt => Cond::Gt,
            CompareOp::Ge => Cond::Ge,
            _ => return Err(Reject::Unsupported),
        };
        if self.hint(act.id, pc) & HINT_GENERIC == 0
            && let Some((a, b, ints)) = self.numbers(act, st, l, r, pc)?
        {
            return Ok(if ints { self.emit(Op::CmpInt(cond, a, b), Rep::Bool) } else { self.emit(Op::CmpFloat(cond, a, b), Rep::Bool) });
        }
        let a = self.real(st, l);
        let b = self.real(st, r);
        self.flush(st);
        let fs = self.state(act, st, pc);
        let v = self.emit(Op::Call(Helper::Compare(op), vec![a, b], Some(fs)), Rep::Bool);
        Self::forget(st);
        Ok(v)
    }

    fn plain_word(&self, v: V) -> bool {
        match self.g.op(v) {
            Op::Word(w) => *w >= ValueBits::BOXED_MIN && (*w >> 48) != (0xFFF0 | (u64::from(j8framework::ValueTag::STRING) + 1)),
            _ => matches!(self.g.rep(v), Rep::Ptr | Rep::Ctx | Rep::Bool),
        }
    }

    fn strict_equals(&mut self, act: &Act<'e>, st: &mut St, l: V, r: V, pc: usize) -> R<V> {
        let (lr, rr) = (self.g.rep(l), self.g.rep(r));
        if matches!(lr, Rep::I32 | Rep::F64) || matches!(rr, Rep::I32 | Rep::F64) {
            if let Some((a, b, ints)) = self.numbers(act, st, l, r, pc)? {
                return Ok(if ints { self.emit(Op::CmpInt(Cond::Eq, a, b), Rep::Bool) } else { self.emit(Op::CmpFloat(Cond::Eq, a, b), Rep::Bool) });
            }
        }
        if Self::is_virtual(st, l) || Self::is_virtual(st, r) {
            return Ok(self.emit(Op::I32(i32::from(l == r)), Rep::Bool));
        }
        if self.plain_word(l) || self.plain_word(r) {
            let a = self.to_word(l);
            let b = self.to_word(r);
            return Ok(self.emit(Op::CmpWord(Cond::Eq, a, b), Rep::Bool));
        }
        let a = self.to_word(l);
        let b = self.to_word(r);
        Ok(self.emit(Op::Call(Helper::StrictEquals, vec![a, b], None), Rep::Bool))
    }

    fn truthy(&mut self, st: &mut St, v: V) -> V {
        match self.g.rep(v) {
            Rep::Bool => v,
            Rep::I32 => {
                let z = self.int(0);
                self.emit(Op::CmpInt(Cond::Ne, v, z), Rep::Bool)
            }
            Rep::F64 => {
                let z = self.emit(Op::F64(0.0), Rep::F64);
                self.emit(Op::CmpFloat(Cond::Ne, v, z), Rep::Bool)
            }
            Rep::Ptr | Rep::Ctx => self.emit(Op::I32(1), Rep::Bool),
            _ => {
                if Self::is_virtual(st, v) {
                    return self.emit(Op::I32(1), Rep::Bool);
                }
                match self.const_word(v) {
                    Some(ValueBits::TRUE) => self.emit(Op::I32(1), Rep::Bool),
                    Some(ValueBits::FALSE | ValueBits::UNDEFINED | ValueBits::NULL) => self.emit(Op::I32(0), Rep::Bool),
                    _ => self.emit(Op::Call(Helper::ToBoolean, vec![v], None), Rep::Bool),
                }
            }
        }
    }

    fn closure_env_on(&mut self, st: &mut St, f: V) -> V {
        let made = st.heap.known.iter().find_map(|x| match x {
            Fact::Env(p, c) if *p == f => Some(*c),
            _ => None,
        });
        if let Some(c) = made {
            return c;
        }
        let fast = self.load(st, LOAD_PTR, f, Self::off(Layout::OBJECT_CALL));
        self.load(st, LOAD_RAW, fast, Self::off(Layout::FAST_CLOSURE + Layout::CLOSURE_ENV))
    }

    fn closure_env(&mut self, f: V) -> V {
        let fast = self.emit(Op::LoadPtr(f, Self::off(Layout::OBJECT_CALL)), Rep::Ptr);
        self.emit(Op::LoadRaw(fast, Self::off(Layout::FAST_CLOSURE + Layout::CLOSURE_ENV)), Rep::Ctx)
    }

    fn off(x: usize) -> i32 {
        i32::try_from(x).unwrap_or(i32::MAX)
    }

    fn ctx_ptr(&mut self, v: V) -> V {
        match self.g.rep(v) {
            Rep::Ctx => v,
            _ => self.emit(Op::Untag(v), Rep::Ctx),
        }
    }

    fn distinct(&mut self, act: &Act<'e>, st: &mut St, a: V, b: V, pc: usize) {
        if st.heap.known.contains(&Fact::Distinct(a, b)) || st.heap.known.contains(&Fact::Distinct(b, a)) {
            return;
        }
        let fs = self.state(act, st, pc);
        let eq = self.emit(Op::CmpWord(Cond::Eq, a, b), Rep::Bool);
        self.emit(Op::Check(eq, false, fs), Rep::None);
        st.heap.known.push(Fact::Distinct(a, b));
    }

    fn load_slot(&mut self, act: &Act<'e>, st: &mut St, ctx: V, idx: u32, pc: usize) -> V {
        if let Some(m) = st.heap.mem.iter().find(|m| m.ctx == ctx && m.idx == idx) {
            return m.val;
        }
        let alias = self.hint(act.id, pc) & HINT_GENERIC != 0;
        for m in st.heap.mem.clone() {
            if m.idx == idx && m.ctx != ctx {
                if alias {
                    if m.dirty {
                        self.write_back(st, &m, None);
                    }
                } else {
                    self.distinct(act, st, m.ctx, ctx, pc);
                }
            }
        }
        let cell = self.emit(Op::ContextCell(ctx, idx), Rep::Ptr);
        let v = self.emit(Op::LoadWord(cell, 0), Rep::Word);
        st.heap.mem.push(Mem { ctx, idx, val: v, dirty: false });
        v
    }

    fn store_slot(&mut self, act: &Act<'e>, st: &mut St, ctx: V, idx: u32, v: V, pc: usize) {
        let alias = self.hint(act.id, pc) & HINT_GENERIC != 0;
        let others: Vec<Mem> = st.heap.mem.iter().filter(|m| m.idx == idx && m.ctx != ctx).cloned().collect();
        for m in others {
            if alias {
                if m.dirty {
                    self.write_back(st, &m, None);
                }
                st.heap.mem.retain(|x| !(x.idx == idx && x.ctx == m.ctx));
            } else {
                self.distinct(act, st, m.ctx, ctx, pc);
            }
        }
        match st.heap.mem.iter_mut().find(|m| m.ctx == ctx && m.idx == idx) {
            Some(m) => {
                m.val = v;
                m.dirty = true;
            }
            None => st.heap.mem.push(Mem { ctx, idx, val: v, dirty: true }),
        }
    }

    fn context_up(&mut self, ctx: V, depth: OperandValue) -> V {
        let mut c = ctx;
        for _ in 0..depth {
            c = self.emit(Op::LoadRaw(c, Self::off(Layout::ENV_PARENT)), Rep::Ctx);
        }
        c
    }

    fn prop_cell(&mut self, o: V, slot: usize) -> V {
        if slot < Layout::INLINE_PROPS {
            o
        } else {
            self.emit(Op::StoreCellAddr(o, u32::try_from(slot).unwrap_or(0)), Rep::Ptr)
        }
    }

    fn prop_off(slot: usize) -> i32 {
        if slot < Layout::INLINE_PROPS { Self::off(Layout::OBJECT_INLINE + slot * Layout::CELL) } else { 0 }
    }

    fn keep(&mut self, s: &Option<ShapeKey>) {
        if let Some(s) = s {
            self.keep.push(*s);
        }
    }

    fn object(&mut self, act: &Act<'e>, st: &mut St, v: V, pc: usize) -> V {
        match self.g.rep(v) {
            Rep::Ptr => v,
            _ => {
                if let Some(Fact::Object(_, p)) = st.heap.known.iter().find(|f| matches!(f, Fact::Object(w, _) if *w == v)) {
                    let p = *p;
                    Self::replace(st, v, p);
                    return p;
                }
                let fs = self.state(act, st, pc);
                let p = self.emit(Op::CheckObject(v, fs), Rep::Ptr);
                Self::replace(st, v, p);
                st.heap.known.push(Fact::Object(v, p));
                if let Some(c) = st.heap.known.iter().find_map(|f| match f {
                    Fact::Env(w, c) if *w == v => Some(*c),
                    _ => None,
                }) {
                    st.heap.known.push(Fact::Env(p, c));
                }
                p
            }
        }
    }

    fn get_named(&mut self, act: &Act<'e>, st: &mut St, obj: V, name: OperandValue, slot: OperandValue, pc: usize) -> R<V> {
        let fb = act.f.property(slot);
        let key = act.f.name(name);
        if Self::vobj(st, obj).is_none()
            && self.hint(act.id, pc) & HINT_GENERIC == 0
            && let Some(kind) = act.f.string_property(slot)
        {
            match kind {
                j8executor::StringFeedback::Length => return Ok(self.string_length(act, st, obj, pc)),
                j8executor::StringFeedback::Proto(fb) if fb.to.is_none() && (fb.shape.is_some() || fb.depth >= 1) => return Ok(self.string_proto_load(act, st, obj, &fb, key, pc)),
                j8executor::StringFeedback::Proto(_) => {
                    let w = self.real(st, obj);
                    return self.generic_get(act, st, w, name, pc);
                }
            }
        }
        if let Some(o) = Self::vobj(st, obj).cloned() {
            if let Some(i) = o.shape.and_then(|s| unsafe { s.shape() }.find(key)) {
                return Ok(o.fields[i]);
            }
            if let Some(fb) = &fb
                && fb.depth >= 1
                && Self::same_shape(&o.shape, &fb.shape)
                && self.hint(act.id, pc) & HINT_GENERIC == 0
            {
                return Ok(self.proto_load(act, st, o.proto, fb, key, pc));
            }
            let w = self.real(st, obj);
            return self.generic_get(act, st, w, name, pc);
        }
        match fb {
            Some(fb) if self.hint(act.id, pc) & HINT_GENERIC == 0 && fb.to.is_none() && (fb.shape.is_some() || fb.depth >= 1) => {
                let p = self.object(act, st, obj, pc);
                self.shape_check(act, st, p, fb.shape, pc);
                if fb.depth == 0 {
                    let c = self.prop_cell(p, fb.slot);
                    let v = self.load(st, LOAD_WORD, c, Self::prop_off(fb.slot));
                    return Ok(self.guard_lazy(act, st, v, key, pc));
                }
                let proto = self.load(st, LOAD_PTR, p, Self::off(Layout::OBJECT_PROTO));
                if fb.shape.is_none() {
                    let fs = self.state(act, st, pc);
                    let z = self.emit(Op::Word(0), Rep::Word);
                    let gone = self.emit(Op::CmpWord(Cond::Eq, proto, z), Rep::Bool);
                    self.emit(Op::Check(gone, false, fs), Rep::None);
                }
                Ok(self.proto_load(act, st, proto, &fb, key, pc))
            }
            _ => {
                let w = self.real(st, obj);
                self.generic_get(act, st, w, name, pc)
            }
        }
    }

    fn string_length(&mut self, act: &Act<'e>, st: &mut St, obj: V, pc: usize) -> V {
        let w = self.real(st, obj);
        let fs = self.state(act, st, pc);
        let p = self.emit(Op::CheckString(w, fs), Rep::Ptr);
        let n = self.emit(Op::Load32(p, Self::off(Layout::STR_UNITS)), Rep::I32);
        let z = self.int(0);
        let neg = self.emit(Op::CmpInt(Cond::Lt, n, z), Rep::Bool);
        self.emit(Op::Check(neg, false, fs), Rep::None);
        n
    }

    fn string_proto_load(&mut self, act: &Act<'e>, st: &mut St, obj: V, fb: &j8executor::PropertyFeedback, name: &str, pc: usize) -> V {
        let w = self.real(st, obj);
        let fs = self.state(act, st, pc);
        self.emit(Op::CheckString(w, fs), Rep::Ptr);
        let proto = self.emit(Op::CtxField(Ctx::STRING_PROTO), Rep::Ptr);
        self.shape_check(act, st, proto, fb.shape, pc);
        if fb.depth == 0 {
            let c = self.prop_cell(proto, fb.slot);
            let v = self.load(st, LOAD_WORD, c, Self::prop_off(fb.slot));
            return self.guard_lazy(act, st, v, name, pc);
        }
        let next = self.load(st, LOAD_PTR, proto, Self::off(Layout::OBJECT_PROTO));
        let fs = self.state(act, st, pc);
        let z = self.emit(Op::Word(0), Rep::Word);
        let gone = self.emit(Op::CmpWord(Cond::Eq, next, z), Rep::Bool);
        self.emit(Op::Check(gone, false, fs), Rep::None);
        self.proto_load(act, st, next, fb, name, pc)
    }

    fn proto_load(&mut self, act: &Act<'e>, st: &mut St, proto: V, fb: &j8executor::PropertyFeedback, name: &str, pc: usize) -> V {
        let mut p = proto;
        for (i, s) in fb.chain.iter().enumerate() {
            self.shape_check(act, st, p, Some(*s), pc);
            if i + 1 < fb.chain.len() {
                p = self.load(st, LOAD_PTR, p, Self::off(Layout::OBJECT_PROTO));
            }
        }
        let c = self.prop_cell(p, fb.slot);
        let v = self.load(st, LOAD_WORD, c, Self::prop_off(fb.slot));
        self.guard_lazy(act, st, v, name, pc)
    }

    fn guard_lazy(&mut self, act: &Act<'e>, st: &mut St, v: V, name: &str, pc: usize) -> V {
        if Lazy::possible(name) {
            let fs = self.state(act, st, pc);
            self.emit(Op::CheckNotLazy(v, fs), Rep::None);
        }
        v
    }

    fn generic_get(&mut self, act: &Act<'e>, st: &mut St, w: V, name: OperandValue, pc: usize) -> R<V> {
        if self.log {
            std::eprintln!("j8jit: generic get {} at {:?}@{pc}", act.f.name(name), act.id);
        }
        self.flush(st);
        let fs = self.state(act, st, pc);
        let v = self.emit(Op::Call(Helper::GetNamed(act.id, u32::try_from(name).unwrap_or(0)), vec![w], Some(fs)), Rep::Word);
        Self::forget(st);
        Ok(v)
    }

    #[expect(clippy::too_many_arguments, reason = "a store's parts")]
    fn set_named(&mut self, act: &Act<'e>, st: &mut St, obj: V, name: OperandValue, slot: OperandValue, v: V, assign: bool, pc: usize) -> R<()> {
        let fb = act.f.property(slot);
        if let Some(o) = Self::vobj(st, obj).cloned()
            && let Some(fb) = &fb
            && self.hint(act.id, pc) & HINT_GENERIC == 0
            && Self::same_shape(&o.shape, &fb.shape)
        {
            match &fb.to {
                Some(to) if fb.slot == o.fields.len() => {
                    if assign {
                        let mut p = o.proto;
                        for (i, s) in fb.chain.iter().enumerate() {
                            self.shape_check(act, st, p, Some(*s), pc);
                            p = self.load(st, LOAD_PTR, p, Self::off(Layout::OBJECT_PROTO));
                            if i + 1 == fb.chain.len() && !st.heap.known.contains(&Fact::NoProto(p)) {
                                let fs = self.state(act, st, pc);
                                let z = self.emit(Op::Word(0), Rep::Word);
                                let none = self.emit(Op::CmpWord(Cond::Eq, p, z), Rep::Bool);
                                self.emit(Op::Check(none, true, fs), Rep::None);
                                st.heap.known.push(Fact::NoProto(p));
                            }
                        }
                        if fb.chain.is_empty() {
                            return self.generic_set(act, st, obj, name, v, assign, pc);
                        }
                    }
                    self.keep(&Some(*to));
                    if let Some((_, x)) = st.heap.objs.iter_mut().find(|(id, _)| *id == obj) {
                        x.shape = Some(*to);
                        x.fields.push(v);
                    }
                    return Ok(());
                }
                None if fb.slot < o.fields.len() => {
                    if let Some((_, x)) = st.heap.objs.iter_mut().find(|(id, _)| *id == obj) {
                        x.fields[fb.slot] = v;
                    }
                    return Ok(());
                }
                _ => {}
            }
        }
        if !Self::is_virtual(st, obj)
            && let Some(fb) = &fb
            && fb.to.is_none()
            && fb.shape.is_some()
            && self.hint(act.id, pc) & HINT_GENERIC == 0
        {
            let p = self.object(act, st, obj, pc);
            self.shape_check(act, st, p, fb.shape, pc);
            let w = self.real(st, v);
            let c = self.prop_cell(p, fb.slot);
            self.emit(Op::StoreCell(c, Self::prop_off(fb.slot), w), Rep::None);
            Self::stored(st);
            return Ok(());
        }
        self.generic_set(act, st, obj, name, v, assign, pc)
    }

    #[expect(clippy::too_many_arguments, reason = "a store's parts")]
    fn generic_set(&mut self, act: &Act<'e>, st: &mut St, obj: V, name: OperandValue, v: V, assign: bool, pc: usize) -> R<()> {
        if self.log {
            std::eprintln!("j8jit: generic set {} at {:?}@{pc} (virtual {})", act.f.name(name), act.id, Self::is_virtual(st, obj));
            if let Some(fb) = act.f.property(act.f.insn(pc).operands[2]) {
                let vo = Self::vobj(st, obj);
                std::eprintln!("   fb shape {:?} slot {} depth {} chain {} to {:?}; virtual shape {:?} fields {:?}", fb.shape.map(ShapeKey::id), fb.slot, fb.depth, fb.chain.len(), fb.to.map(ShapeKey::id), vo.map(|o| o.shape.map(ShapeKey::id)), vo.map(|o| o.fields.len()));
            }
        }
        let o = self.real(st, obj);
        let w = self.real(st, v);
        self.flush(st);
        let fs = self.state(act, st, pc);
        self.emit(Op::Call(Helper::SetNamed(act.id, u32::try_from(name).unwrap_or(0), assign), vec![o, w], Some(fs)), Rep::None);
        Self::forget(st);
        Ok(())
    }

    #[expect(clippy::too_many_arguments, reason = "a call's parts")]
    fn call(&mut self, act: &mut Act<'e>, st: &mut St, callee: V, this: V, args: Vec<V>, fb: OperandValue, pc: usize) -> R<V> {
        if self.hint(act.id, pc) & HINT_GENERIC == 0
            && let Some(d) = self.known_direct(act, fb, args.len())
        {
            return Ok(self.direct_call(act, st, callee, this, args, d, pc));
        }
        let target = self.known_target(act, st, callee, fb);
        if let Some(t) = target
            && self.hint(act.id, pc) & HINT_GENERIC == 0
        {
            let tf = self.exe.jit_function(t);
            self.codes.push(t);
            if tf.plain() && !tf.class_constructor()
                && let Some(this) = self.receiver(st, tf, this)
            {
                let p = self.checked_callee(act, st, callee, t, pc);
                if self.inlinable(act, tf, pc) {
                    return self.inline(act, st, p, tf, this, args, None, pc);
                }
                if args.len() <= 3 {
                    let cw = if self.g.rep(callee) == Rep::Word { callee } else { self.to_word(p) };
                    let tw = self.real(st, this);
                    let aw: Vec<V> = args.iter().map(|a| self.real(st, *a)).collect();
                    self.flush(st);
                    let fs = self.state(act, st, pc);
                    let k = self.target(t);
                    let v = self.emit(Op::CallJit(k, cw, tw, aw, fs), Rep::Word);
                    Self::forget(st);
                    if self.hint(act.id, pc + 1) & HINT_FLOAT == 0 {
                        let mut after = st.clone();
                        after.r.acc = v;
                        let fs = self.state(act, &after, pc + 1);
                        return Ok(self.emit(Op::UnboxInt(v, fs), Rep::I32));
                    }
                    return Ok(v);
                }
            }
        }
        if args.len() + 2 > crate::runtime::ARGS_WORDS {
            return Err(Reject::Unsupported);
        }
        let cw = self.real(st, callee);
        let tw = self.real(st, this);
        let mut ins = vec![cw, tw];
        for a in args {
            ins.push(self.real(st, a));
        }
        self.flush(st);
        let fs = self.state(act, st, pc);
        let v = self.emit(Op::Call(Helper::Call, ins, Some(fs)), Rep::Word);
        Self::forget(st);
        Ok(v)
    }

    fn known_direct(&self, act: &Act<'e>, fb: OperandValue, arity: usize) -> Option<Direct> {
        let native = u64::try_from(act.f.call_native(fb)?).ok()?;
        let arity = u8::try_from(arity).ok()?;
        self.directs.iter().copied().find(|d| d.native == native && d.arity == arity)
    }

    #[expect(clippy::too_many_arguments, reason = "a call's parts")]
    fn direct_call(&mut self, act: &mut Act<'e>, st: &mut St, callee: V, this: V, args: Vec<V>, d: Direct, pc: usize) -> V {
        let cw = self.real(st, callee);
        self.guard_native(act, st, callee, d.native, pc);
        let tw = self.real(st, this);
        let mut ins = vec![cw, tw];
        for a in args {
            ins.push(self.real(st, a));
        }
        self.flush(st);
        let fs = self.state(act, st, pc);
        let h = if d.inline_push && d.arity == 1 { Helper::InlinePush(d.index) } else { Helper::Direct(d.index, d.out) };
        let v = self.emit(Op::Call(h, ins, Some(fs)), h.rep());
        Self::forget(st);
        v
    }

    fn guard_native(&mut self, act: &Act<'e>, st: &mut St, callee: V, native: u64, pc: usize) {
        let p = self.object(act, st, callee, pc);
        let fs = self.state(act, st, pc);
        self.emit(Op::CheckNative(p, native, fs), Rep::None);
    }

    fn checked_callee(&mut self, act: &Act<'e>, st: &mut St, callee: V, t: FuncId, pc: usize) -> V {
        if let Op::Global(k, None, fs) = self.g.op(callee).clone() {
            self.g.insts[callee as usize].op = Op::Global(k, Some(t), fs);
            let p = self.emit(Op::Untag(callee), Rep::Ptr);
            Self::replace(st, callee, p);
            return p;
        }
        if let Op::Global(_, Some(x), _) = self.g.op(callee)
            && *x == t
        {
            return self.emit(Op::Untag(callee), Rep::Ptr);
        }
        let p = self.object(act, st, callee, pc);
        if !st.heap.known.contains(&Fact::Func(p, t)) {
            let fs = self.state(act, st, pc);
            self.emit(Op::CheckFunc(p, t, fs), Rep::None);
            st.heap.known.push(Fact::Func(p, t));
        }
        p
    }

    fn target(&mut self, t: FuncId) -> u32 {
        match self.targets.iter().position(|x| *x == t) {
            Some(i) => u32::try_from(i).unwrap_or(0),
            None => {
                self.targets.push(t);
                u32::try_from(self.targets.len() - 1).unwrap_or(0)
            }
        }
    }

    fn receiver(&mut self, st: &St, tf: Function<'_>, this: V) -> Option<V> {
        if !tf.wraps_this() || matches!(self.g.rep(this), Rep::Ptr) || Self::is_virtual(st, this) {
            return Some(this);
        }
        match self.const_word(this) {
            Some(ValueBits::UNDEFINED | ValueBits::NULL) => {
                let g = self.emit(Op::CtxField(Ctx::GLOBAL), Rep::Ptr);
                Some(g)
            }
            _ => None,
        }
    }

    fn known_target(&self, act: &Act<'e>, _st: &St, _callee: V, fb: OperandValue) -> Option<FuncId> {
        act.f.call_target(fb).filter(|&t| self.exe.jit_live_function(t).is_some())
    }

    fn inlinable(&self, act: &Act<'e>, tf: Function<'_>, pc: usize) -> bool {
        if act.depth + 1 >= INLINE_DEPTH || tf.len() > INLINE_SIZE || self.hint(act.id, pc) & HINT_NO_INLINE != 0 {
            return false;
        }
        let mut c = act.caller.clone();
        let mut seen = usize::from(tf.id() == act.id);
        while let Some(x) = c {
            seen += usize::from(x.func == tf.id());
            c = x.parent.clone();
        }
        if seen > RECURSIVE_INLINE {
            return false;
        }
        self.cfgs.get(&tf.id().0).map_or_else(|| Cfg::new(tf).ok, |c| c.ok)
    }

    #[expect(clippy::too_many_arguments, reason = "a call's parts")]
    fn inline(&mut self, act: &mut Act<'e>, st: &mut St, fp: V, tf: Function<'e>, this: V, args: Vec<V>, construct: Option<(V, V)>, pc: usize) -> R<V> {
        let cfg = self.cfg(tf);
        let fw = fp;
        let caller = Rc::new(Caller { func: act.id, pc: u32::try_from(pc + 1).unwrap_or(0), callee: act.callee, env: act.env, argc: act.argc, r: st.r.clone(), construct: act.construct, parent: act.caller.clone() });
        let mut params = vec![this];
        for i in 0..cfg.params.saturating_sub(1) {
            params.push(args.get(i).copied().unwrap_or(self.undef));
        }
        let ctx = self.closure_env_on(st, fp);
        let mut regs = Regs { params, regs: vec![self.undef; cfg.regs], acc: self.undef, ctx };
        if let (Some((nt, _)), Some(r)) = (construct, tf.shared().bytecode.new_target_or_generator)
            && let Some(slot) = usize::try_from(r).ok().and_then(|r| regs.regs.get_mut(r))
        {
            *slot = nt;
        }
        let inner = St { r: regs, heap: core::mem::take(&mut st.heap) };
        let regions = vec![Region { start: 0, end: tf.len().saturating_sub(1), peel: None }];
        let mut a = self.activation(tf, fw, u32::try_from(args.len()).unwrap_or(0), construct, Some(caller), act.depth + 1, regions);
        a.env = ctx;
        let start = self.jump(u32::MAX);
        a.edges.entry((0, 0)).or_default().push((start, inner));
        if self.walk(&mut a).is_err() {
            if self.log { std::eprintln!("j8jit: restart: inline restart"); }
            self.restart = true;
            return Err(Reject::Unsupported);
        }
        let rets = core::mem::take(&mut a.rets);
        if rets.is_empty() {
            self.cur = None;
            return Ok(self.undef);
        }
        let join = self.g.add_block();
        for (b, _, _) in &rets {
            self.retarget(*b, join);
        }
        self.g.blocks[join as usize].preds = rets.iter().map(|(b, _, _)| *b).collect();
        let mut edges: Vec<(B, St)> = rets.iter().map(|(b, v, s)| {
            let mut s = s.clone();
            s.r = st.r.clone();
            s.r.acc = *v;
            (*b, s)
        }).collect();
        if edges.len() == 1 {
            let (_, s) = edges.pop().expect("one edge");
            self.cur = Some(join);
            st.heap = s.heap;
            return Ok(s.r.acc);
        }
        let live = {
            let ccfg = act.cfg.clone();
            let mut b = ccfg.live[pc + 1].clone();
            let _ = &mut b;
            b
        };
        let _ = live;
        let accs: Vec<V> = edges.iter().map(|(_, s)| s.r.acc).collect();
        for (p, s) in &mut edges {
            let v = s.r.acc;
            if Self::is_virtual(s, v) && !accs.iter().all(|x| *x == v) {
                self.materialize(s, v, Some(*p));
            }
        }
        let accs: Vec<V> = edges.iter().map(|(_, s)| s.r.acc).collect();
        let rep = accs.iter().map(|v| self.g.rep(*v)).reduce(Self::join).unwrap_or(Rep::Word);
        let rep = if rep == Rep::Bool { Rep::Word } else { rep };
        let acc = if accs.iter().all(|x| *x == accs[0]) {
            accs[0]
        } else {
            let ins: Vec<V> = edges.iter().zip(&accs).map(|((p, _), v)| self.convert_at(*p, *v, rep)).collect();
            self.g.push_phi(join, ins, rep)
        };
        let mut mem = Vec::new();
        for m in &edges[0].1.heap.mem {
            if edges.iter().all(|(_, s)| s.heap.mem.iter().any(|x| x.ctx == m.ctx && x.idx == m.idx && x.val == m.val)) {
                mem.push(m.clone());
            }
        }
        for (p, s) in &mut edges {
            for m in s.heap.mem.clone() {
                if m.dirty && !mem.iter().any(|x: &Mem| x.ctx == m.ctx && x.idx == m.idx) {
                    self.write_back(s, &m, Some(*p));
                }
            }
        }
        let mut objs = Vec::new();
        for (id, o) in &edges[0].1.heap.objs {
            if edges.iter().all(|(_, s)| Self::vobj(s, *id).is_some_and(|x| x.fields == o.fields && x.proto == o.proto && Self::same_shape(&x.shape, &o.shape))) {
                objs.push((*id, o.clone()));
            }
        }
        for (p, s) in &mut edges {
            let ids: Vec<V> = s.heap.objs.iter().map(|(id, _)| *id).collect();
            for id in ids {
                if !objs.iter().any(|(x, _)| *x == id) && Self::reachable(s, id) {
                    self.materialize(s, id, Some(*p));
                }
            }
        }
        let mut known = edges[0].1.heap.known.clone();
        known.retain(|f| edges.iter().all(|(_, s)| s.heap.known.contains(f)));
        st.heap = Heap { objs, mem, known };
        self.cur = Some(join);
        Ok(acc)
    }

    #[expect(clippy::too_many_arguments, reason = "a construct's parts")]
    fn construct(&mut self, act: &mut Act<'e>, st: &mut St, callee: V, nt: V, args: Vec<V>, fb: OperandValue, pc: usize) -> R<V> {
        let target = act.f.call_target(fb).filter(|&t| self.exe.jit_live_function(t).is_some());
        if let Some(t) = target
            && self.hint(act.id, pc) & (HINT_GENERIC | HINT_NO_INLINE) == 0
            && callee == nt
        {
            let tf = self.exe.jit_function(t);
            self.codes.push(t);
            let returns_undefined = (0..tf.len()).all(|i| tf.insn(i).bytecode != Bytecode::Return || (i > 0 && tf.insn(i - 1).bytecode == Bytecode::LdaUndefined));
            if (tf.constructs() || tf.class_constructor()) && !tf.derived() && returns_undefined && self.inlinable(act, tf, pc)
                && let Some(pfb) = tf.prototype()
                && pfb.depth == 0
                && pfb.to.is_none()
                && pfb.shape.is_some()
            {
                let p = self.checked_callee(act, st, callee, t, pc);
                self.shape_check(act, st, p, pfb.shape, pc);
                let c = self.prop_cell(p, pfb.slot);
                let pw = self.load(st, LOAD_WORD, c, Self::prop_off(pfb.slot));
                let pw = self.guard_lazy(act, st, pw, "prototype", pc);
                let proto = self.object(act, st, pw, pc);
                let id = self.emit(Op::Word(0), Rep::Ptr);
                st.heap.objs.push((id, VObj { proto, shape: None, fields: Vec::new() }));
                let ntw = self.to_word(p);
                return self.inline(act, st, p, tf, id, args, Some((ntw, id)), pc);
            }
        }
        if args.len() + 2 > crate::runtime::ARGS_WORDS {
            return Err(Reject::Unsupported);
        }
        let cw = self.real(st, callee);
        let nw = self.real(st, nt);
        let mut ins = vec![cw, nw];
        for a in args {
            ins.push(self.real(st, a));
        }
        self.flush(st);
        let fs = self.state(act, st, pc);
        let v = self.emit(Op::Call(Helper::Construct, ins, Some(fs)), Rep::Word);
        Self::forget(st);
        Ok(v)
    }

    fn insn(&mut self, act: &mut Act<'e>, st: &mut St, pc: usize, d: j8executor::JitInsn<'e>) -> R<Flow> {
        use Bytecode as Bc;
        let o = d.operands;
        let acc = st.r.acc;
        macro_rules! set_acc {
            ($v:expr) => {{
                let v = $v;
                st.r.acc = v;
            }};
        }
        match d.bytecode {
            Bc::LdaZero => set_acc!(self.int(0)),
            Bc::LdaSmi => set_acc!(self.int(i32::try_from(o[0]).unwrap_or(0))),
            Bc::LdaUndefined => set_acc!(self.undef),
            Bc::LdaNull => set_acc!(self.word(ValueBits::NULL)),
            Bc::LdaTheHole => set_acc!(self.word(ValueBits::UNINIT)),
            Bc::LdaTrue => set_acc!(self.word(ValueBits::TRUE)),
            Bc::LdaFalse => set_acc!(self.word(ValueBits::FALSE)),
            Bc::LdaConstant => match act.f.constant(o[0]) {
                Some(Constant::Number(x)) => {
                    #[expect(clippy::cast_possible_truncation, reason = "checked by converting back")]
                    let i = *x as i32;
                    let v = if f64::from(i) == *x && !(i == 0 && x.is_sign_negative()) { self.int(i) } else { self.emit(Op::F64(*x), Rep::F64) };
                    set_acc!(v);
                }
                Some(Constant::String(s)) if s.is_empty() => set_acc!(self.string_empty()),
                Some(Constant::String(s)) if s.len() == 1 && s.as_bytes()[0] < 128 => set_acc!(self.string_byte(s.as_bytes()[0])),
                Some(Constant::Utf16(u)) if u.is_empty() => set_acc!(self.string_empty()),
                Some(Constant::Utf16(u)) if u.len() == 1 && u[0] < 128 => set_acc!(self.string_byte(u8::try_from(u[0]).unwrap_or(0))),
                Some(Constant::String(_) | Constant::Utf16(_)) => {
                    let v = self.emit(Op::Call(Helper::Constant(act.id, u32::try_from(o[0]).unwrap_or(0)), Vec::new(), None), Rep::Word);
                    set_acc!(v);
                }
                _ => return Err(Reject::Unsupported),
            },
            Bc::Ldar => set_acc!(Self::reg(act, st, o[0])?),
            Bc::Star => Self::set_reg(act, st, o[0], acc)?,
            Bc::Mov => {
                let v = Self::reg(act, st, o[0])?;
                Self::set_reg(act, st, o[1], v)?;
            }
            Bc::Add | Bc::Sub | Bc::Mul | Bc::Div | Bc::Mod | Bc::BitwiseAnd | Bc::BitwiseOr | Bc::BitwiseXor | Bc::ShiftLeft | Bc::ShiftRight | Bc::ShiftRightLogical => {
                let l = Self::reg(act, st, o[0])?;
                let op = Self::binary_op(d.bytecode);
                let v = if d.bytecode == Bc::Add && self.hint(act.id, pc) & HINT_GENERIC != 0 { self.concat_values(act, st, l, st.r.acc, pc) } else { self.arith(act, st, op, l, st.r.acc, pc)? };
                set_acc!(v);
            }
            Bc::AddSmi | Bc::SubSmi | Bc::MulSmi | Bc::DivSmi | Bc::ModSmi | Bc::BitwiseAndSmi | Bc::BitwiseOrSmi | Bc::BitwiseXorSmi | Bc::ShiftLeftSmi | Bc::ShiftRightSmi | Bc::ShiftRightLogicalSmi => {
                let r = self.int(i32::try_from(o[0]).unwrap_or(0));
                let op = Self::binary_op(d.bytecode);
                let v = if d.bytecode == Bc::AddSmi && self.hint(act.id, pc) & HINT_GENERIC != 0 { self.concat_values(act, st, acc, r, pc) } else { self.arith(act, st, op, acc, r, pc)? };
                set_acc!(v);
            }
            Bc::Inc | Bc::Dec => {
                let one = self.int(1);
                let op = if d.bytecode == Bc::Inc { BinaryOp::Add } else { BinaryOp::Sub };
                let v = self.arith(act, st, op, acc, one, pc)?;
                set_acc!(v);
            }
            Bc::TestLessThan | Bc::TestGreaterThan | Bc::TestLessThanOrEqual | Bc::TestGreaterThanOrEqual => {
                let l = Self::reg(act, st, o[0])?;
                let op = match d.bytecode {
                    Bc::TestLessThan => CompareOp::Lt,
                    Bc::TestGreaterThan => CompareOp::Gt,
                    Bc::TestLessThanOrEqual => CompareOp::Le,
                    _ => CompareOp::Ge,
                };
                let v = self.compare(act, st, op, l, acc, pc)?;
                set_acc!(v);
            }
            Bc::TestEqualStrict => {
                let l = Self::reg(act, st, o[0])?;
                let v = self.strict_equals(act, st, l, acc, pc)?;
                set_acc!(v);
            }
            Bc::TestEqual => {
                let l = Self::reg(act, st, o[0])?;
                let num = |r: Rep| matches!(r, Rep::I32 | Rep::F64);
                let v = if num(self.g.rep(l)) && num(self.g.rep(acc)) {
                    self.strict_equals(act, st, l, acc, pc)?
                } else {
                    self.helper_call(act, st, Helper::LooseEquals, &[l, acc], pc)
                };
                set_acc!(v);
            }
            Bc::TestReferenceEqual => {
                let l = Self::reg(act, st, o[0])?;
                let (a, b) = (self.real(st, l), self.real(st, acc));
                set_acc!(self.emit(Op::CmpWord(Cond::Eq, a, b), Rep::Bool));
            }
            Bc::TestInstanceOf => {
                let l = Self::reg(act, st, o[0])?;
                set_acc!(self.helper_call(act, st, Helper::InstanceOf, &[l, acc], pc));
            }
            Bc::Negate => {
                let v = match self.as_float(act, st, acc, pc)? {
                    Some(x) => {
                        let m = self.emit(Op::F64(-1.0), Rep::F64);
                        self.emit(Op::Float(FloatOp::Mul, x, m), Rep::F64)
                    }
                    None => self.helper_call(act, st, Helper::Unary(j8framework::UnaryOp::Neg), &[acc], pc),
                };
                set_acc!(v);
            }
            Bc::BitwiseNot => {
                let v = match self.as_int(act, st, acc, pc)? {
                    Some(x) => {
                        let m = self.int(-1);
                        self.emit(Op::Int(IntOp::Xor, x, m, None), Rep::I32)
                    }
                    None => self.helper_call(act, st, Helper::Unary(j8framework::UnaryOp::BitNot), &[acc], pc),
                };
                set_acc!(v);
            }
            Bc::TypeOf => set_acc!(self.helper_call(act, st, Helper::Unary(j8framework::UnaryOp::Typeof), &[acc], pc)),
            Bc::ToBoolean => {
                let f = self.truthy(st, acc);
                set_acc!(f);
            }
            Bc::GetKeyedProperty => {
                let obj = Self::reg(act, st, o[0])?;
                set_acc!(self.helper_call(act, st, Helper::GetKeyed, &[obj, acc], pc));
            }
            Bc::SetKeyedProperty => {
                let obj = Self::reg(act, st, o[0])?;
                let key = Self::reg(act, st, o[1])?;
                let strict = act.f.strict();
                self.helper_call(act, st, Helper::SetKeyed(strict), &[obj, key, acc], pc);
            }
            Bc::TestUndetectable | Bc::TestNull | Bc::TestUndefined => {
                let w = match d.bytecode {
                    Bc::TestNull => ValueBits::NULL,
                    _ => ValueBits::UNDEFINED,
                };
                if d.bytecode == Bc::TestUndetectable {
                    return Err(Reject::Unsupported);
                }
                let a = self.to_word(acc);
                let k = self.word(w);
                set_acc!(self.emit(Op::CmpWord(Cond::Eq, a, k), Rep::Bool));
            }
            Bc::LogicalNot | Bc::ToBooleanLogicalNot => {
                let b = if d.bytecode == Bc::LogicalNot && self.g.rep(acc) == Rep::Bool {
                    acc
                } else if d.bytecode == Bc::LogicalNot {
                    let a = self.to_word(acc);
                    let t = self.word(ValueBits::TRUE);
                    self.emit(Op::CmpWord(Cond::Eq, a, t), Rep::Bool)
                } else {
                    self.truthy(st, acc)
                };
                set_acc!(self.not(b));
            }
            Bc::Jump | Bc::JumpConstant => return Ok(Flow::Jump(d.target)),
            Bc::JumpLoop => {
                let fs = self.state(act, st, d.target);
                self.emit(Op::Poll(fs), Rep::None);
                return Ok(Flow::Loop(d.target));
            }
            Bc::JumpIfTrue | Bc::JumpIfTrueConstant | Bc::JumpIfFalse | Bc::JumpIfFalseConstant => {
                let on_true = matches!(d.bytecode, Bc::JumpIfTrue | Bc::JumpIfTrueConstant);
                let flag = if self.g.rep(acc) == Rep::Bool {
                    acc
                } else {
                    let a = self.to_word(acc);
                    let t = self.word(ValueBits::TRUE);
                    self.emit(Op::CmpWord(Cond::Eq, a, t), Rep::Bool)
                };
                return Ok(Flow::Branch(flag, d.target, on_true));
            }
            Bc::JumpIfToBooleanTrue | Bc::JumpIfToBooleanTrueConstant | Bc::JumpIfToBooleanFalse | Bc::JumpIfToBooleanFalseConstant => {
                let flag = self.truthy(st, acc);
                return Ok(Flow::Branch(flag, d.target, matches!(d.bytecode, Bc::JumpIfToBooleanTrue | Bc::JumpIfToBooleanTrueConstant)));
            }
            Bc::JumpIfUndefined | Bc::JumpIfUndefinedConstant | Bc::JumpIfNotUndefined | Bc::JumpIfNotUndefinedConstant | Bc::JumpIfNull | Bc::JumpIfNullConstant | Bc::JumpIfNotNull | Bc::JumpIfNotNullConstant => {
                let w = if matches!(d.bytecode, Bc::JumpIfNull | Bc::JumpIfNullConstant | Bc::JumpIfNotNull | Bc::JumpIfNotNullConstant) { ValueBits::NULL } else { ValueBits::UNDEFINED };
                let a = self.to_word(acc);
                let k = self.word(w);
                let eq = self.emit(Op::CmpWord(Cond::Eq, a, k), Rep::Bool);
                return Ok(Flow::Branch(eq, d.target, !matches!(d.bytecode, Bc::JumpIfNotUndefined | Bc::JumpIfNotUndefinedConstant | Bc::JumpIfNotNull | Bc::JumpIfNotNullConstant)));
            }
            Bc::Return => {
                let v = if act.construct.is_some() && self.const_word(acc) == Some(ValueBits::UNDEFINED) { st.r.params[0] } else { acc };
                if act.caller.is_some() {
                    let b = self.cur.expect("a live block");
                    self.g.push(b, Op::Jump(u32::MAX), Rep::None);
                    self.cur = None;
                    act.rets.push((b, v, st.clone()));
                    return Ok(Flow::End);
                }
                let w = self.real(st, v);
                self.flush(st);
                self.emit(Op::Return(w, u32::try_from(pc).unwrap_or(0)), Rep::None);
                self.cur = None;
                return Ok(Flow::End);
            }
            Bc::ThrowReferenceErrorIfHole => {
                let fs = self.state(act, st, pc);
                if self.g.rep(acc) == Rep::Word && !Self::is_virtual(st, acc) {
                    match self.const_word(acc) {
                        Some(ValueBits::UNINIT) => {
                            self.emit(Op::Deopt(fs), Rep::None);
                            self.cur = None;
                            return Ok(Flow::End);
                        }
                        Some(_) => {}
                        None => {
                            self.emit(Op::CheckNotHole(acc, fs), Rep::None);
                        }
                    }
                }
            }
            Bc::LdaCurrentContextSlot | Bc::LdaCurrentContextSlotNoCell | Bc::LdaImmutableCurrentContextSlot => {
                let c = self.ctx_ptr(st.r.ctx);
                Self::replace_ctx(st, c);
                let v = self.load_slot(act, st, c, Self::env_index(o[0]), pc);
                set_acc!(v);
            }
            Bc::StaCurrentContextSlot | Bc::StaCurrentContextSlotNoCell => {
                let c = self.ctx_ptr(st.r.ctx);
                Self::replace_ctx(st, c);
                self.store_slot(act, st, c, Self::env_index(o[0]), acc, pc);
            }
            Bc::LdaContextSlot | Bc::LdaContextSlotNoCell | Bc::LdaImmutableContextSlot => {
                let r = Self::reg(act, st, o[0])?;
                let c = self.ctx_ptr(r);
                let c = self.context_up(c, o[2]);
                let v = self.load_slot(act, st, c, Self::env_index(o[1]), pc);
                set_acc!(v);
            }
            Bc::StaContextSlot | Bc::StaContextSlotNoCell => {
                let r = Self::reg(act, st, o[0])?;
                let c = self.ctx_ptr(r);
                let c = self.context_up(c, o[2]);
                self.store_slot(act, st, c, Self::env_index(o[1]), acc, pc);
            }
            Bc::PushContext => {
                let cur = st.r.ctx;
                Self::set_reg(act, st, o[0], cur)?;
                st.r.ctx = acc;
            }
            Bc::PopContext => {
                let v = Self::reg(act, st, o[0])?;
                st.r.ctx = v;
            }
            Bc::CreateFunctionContext if act.f.extension() => return Err(Reject::Unsupported),
            Bc::CreateFunctionContext | Bc::CreateBlockContext => {
                let (shape, n) = match d.bytecode {
                    Bc::CreateFunctionContext => (act.f.scope(o[0]).map(|s| s.shape).or(act.f.function_scope()), u32::try_from(o[1]).unwrap_or(0)),
                    _ => {
                        let s = act.f.scope(o[0]).ok_or(Reject::Unsupported)?;
                        (Some(s.shape), u32::try_from(s.slots).unwrap_or(0))
                    }
                };
                let Some(shape) = shape else { return Err(Reject::Unsupported) };
                let parent = self.to_word(st.r.ctx);
                set_acc!(self.emit(Op::Call(Helper::CreateContext(shape, n), vec![parent], None), Rep::Word));
            }
            Bc::CreateClosure => {
                let c = self.ctx_ptr(st.r.ctx);
                Self::replace_ctx(st, c);
                let parent = self.to_word(c);
                let f = self.emit(Op::Call(Helper::CreateClosure(act.id, u32::try_from(o[0]).unwrap_or(0)), vec![parent], None), Rep::Word);
                st.heap.known.push(Fact::Env(f, c));
                set_acc!(f);
            }
            Bc::LdaGlobal => {
                let v = self.global(act, st, o[0], o[1], pc, false)?;
                set_acc!(v);
            }
            Bc::LdaGlobalInsideTypeof => {
                let v = self.global(act, st, o[0], o[1], pc, true)?;
                set_acc!(v);
            }
            Bc::GetNamedProperty => {
                let obj = Self::reg(act, st, o[0])?;
                let v = self.get_named(act, st, obj, o[1], o[2], pc)?;
                set_acc!(v);
            }
            Bc::SetNamedProperty | Bc::DefineNamedOwnProperty => {
                let obj = Self::reg(act, st, o[0])?;
                self.set_named(act, st, obj, o[1], o[2], acc, d.bytecode == Bc::SetNamedProperty, pc)?;
            }
            Bc::CreateObjectLiteral => {
                let v = self.object_literal(act, st, o[0], pc)?;
                set_acc!(v);
            }
            Bc::CreateRegExpLiteral => {
                let fs = self.state(act, st, pc);
                let v = self.emit(Op::Call(Helper::RegExpLiteral(act.id, u32::try_from(o[0]).unwrap_or(0), u32::try_from(o[2]).unwrap_or(0)), vec![], Some(fs)), Rep::Word);
                set_acc!(v);
            }
            Bc::CallUndefinedReceiver0 | Bc::CallUndefinedReceiver1 | Bc::CallUndefinedReceiver2 => {
                let callee = Self::reg(act, st, o[0])?;
                let n = (d.bytecode as u8 - Bc::CallUndefinedReceiver0 as u8) as usize;
                let args = (0..n).map(|i| Self::reg(act, st, o[1 + i])).collect::<R<Vec<V>>>()?;
                let v = self.call(act, st, callee, self.undef, args, o[1 + n], pc)?;
                if self.cur.is_none() {
                    return Ok(Flow::End);
                }
                set_acc!(v);
            }
            Bc::CallProperty0 | Bc::CallProperty1 | Bc::CallProperty2 => {
                let callee = Self::reg(act, st, o[0])?;
                let this = Self::reg(act, st, o[1])?;
                let n = (d.bytecode as u8 - Bc::CallProperty0 as u8) as usize;
                let args = (0..n).map(|i| Self::reg(act, st, o[2 + i])).collect::<R<Vec<V>>>()?;
                let v = self.call(act, st, callee, this, args, o[2 + n], pc)?;
                if self.cur.is_none() {
                    return Ok(Flow::End);
                }
                set_acc!(v);
            }
            Bc::CallUndefinedReceiver | Bc::CallProperty | Bc::CallAnyReceiver => {
                let callee = Self::reg(act, st, o[0])?;
                let n = usize::try_from(o[2]).unwrap_or(0);
                let list = (0..n).map(|i| Self::reg(act, st, o[1] + i64::try_from(i).unwrap_or(0))).collect::<R<Vec<V>>>()?;
                let (this, args) = if d.bytecode == Bc::CallUndefinedReceiver { (self.undef, list) } else {
                    let mut l = list.into_iter();
                    (l.next().unwrap_or(self.undef), l.collect())
                };
                let v = self.call(act, st, callee, this, args, o[3], pc)?;
                if self.cur.is_none() {
                    return Ok(Flow::End);
                }
                set_acc!(v);
            }
            Bc::Construct => {
                let callee = Self::reg(act, st, o[0])?;
                let n = usize::try_from(o[2]).unwrap_or(0);
                let args = (0..n).map(|i| Self::reg(act, st, o[1] + i64::try_from(i).unwrap_or(0))).collect::<R<Vec<V>>>()?;
                let v = self.construct(act, st, callee, acc, args, o[3], pc)?;
                if self.cur.is_none() {
                    return Ok(Flow::End);
                }
                set_acc!(v);
            }
            _ => return Err(Reject::Unsupported),
        }
        Ok(Flow::Next)
    }

    fn replace_ctx(st: &mut St, c: V) {
        st.r.ctx = c;
    }

    fn not(&mut self, flag: V) -> V {
        let z = self.emit(Op::I32(0), Rep::Bool);
        self.emit(Op::CmpInt(Cond::Eq, flag, z), Rep::Bool)
    }

    fn env_index(slot: OperandValue) -> u32 {
        u32::try_from(slot).unwrap_or(0).saturating_sub(j8bytecode::Context::MIN_SLOTS)
    }

    fn binary_op(b: Bytecode) -> BinaryOp {
        use Bytecode as Bc;
        match b {
            Bc::Add | Bc::AddSmi => BinaryOp::Add,
            Bc::Sub | Bc::SubSmi => BinaryOp::Sub,
            Bc::Mul | Bc::MulSmi => BinaryOp::Mul,
            Bc::Div | Bc::DivSmi => BinaryOp::Div,
            Bc::Mod | Bc::ModSmi => BinaryOp::Rem,
            Bc::BitwiseOr | Bc::BitwiseOrSmi => BinaryOp::BitOr,
            Bc::BitwiseXor | Bc::BitwiseXorSmi => BinaryOp::BitXor,
            Bc::BitwiseAnd | Bc::BitwiseAndSmi => BinaryOp::BitAnd,
            Bc::ShiftLeft | Bc::ShiftLeftSmi => BinaryOp::Shl,
            Bc::ShiftRight | Bc::ShiftRightSmi => BinaryOp::Shr,
            _ => BinaryOp::UShr,
        }
    }

    fn global(&mut self, act: &Act<'e>, st: &mut St, name: OperandValue, slot: OperandValue, pc: usize, typeof_: bool) -> R<V> {
        if self.hint(act.id, pc) & HINT_GENERIC == 0 {
            let nk = u32::try_from(name).unwrap_or(0);
            if let Some(v) = st.heap.known.iter().find_map(|f| match f {
                Fact::Global(func, k, v) if *func == act.id.0 && *k == nk => Some(*v),
                _ => None,
            }) {
                return Ok(v);
            }
            if !typeof_ {
                let k = u32::try_from(self.g.globals.len()).unwrap_or(0);
                self.g.globals.push((act.id, nk, u32::try_from(slot).unwrap_or(0)));
                let fs = self.state(act, st, pc);
                let v = self.emit(Op::Global(k, None, fs), Rep::Word);
                st.heap.known.push(Fact::Global(act.id.0, nk, v));
                return Ok(v);
            }
        }
        let fb = act.f.global(slot);
        if !typeof_
            && let Some(fb) = fb
            && self.hint(act.id, pc) & HINT_GENERIC == 0
        {
            let path = if fb.slot < Layout::INLINE_PROPS { None } else { (self.paths)(fb.global.is_some(), fb.slot) };
            if self.log {
                std::eprintln!("global {} slot {} in global {} path {:?}", act.f.name(name), fb.slot, fb.global.is_some(), path);
            }
            if fb.slot < Layout::INLINE_PROPS || path.is_some() {
                let scope = self.emit(Op::CtxField(Ctx::SCRIPT_SCOPE), Rep::Ptr);
                let fs = self.state(act, st, pc);
                let s = fb.scope;
                self.keep(&s);
                self.emit(Op::CheckShape(scope, s, fs), Rep::None);
                let holder = match &fb.global {
                    Some(g) => {
                        let global = self.emit(Op::CtxField(Ctx::GLOBAL), Rep::Ptr);
                        let fs = self.state(act, st, pc);
                        let s = Some(*g);
                        self.keep(&s);
                        self.emit(Op::CheckShape(global, s, fs), Rep::None);
                        global
                    }
                    None => scope,
                };
                let key = act.f.name(name);
                let Some(path) = path else {
                    let v = self.emit(Op::LoadWord(holder, Self::prop_off(fb.slot)), Rep::Word);
                    return Ok(self.guard_lazy(act, st, v, key, pc));
                };
                let mut p = self.emit(Op::LoadPtr(holder, Self::off(Layout::OBJECT_STORE)), Rep::Ptr);
                let head = self.emit(Op::LoadRaw(p, 0), Rep::Word);
                let want = self.word(path.head as u64);
                let same = self.emit(Op::CmpWord(Cond::Eq, head, want), Rep::Bool);
                let fs = self.state(act, st, pc);
                self.emit(Op::Check(same, true, fs), Rep::None);
                for k in &path.kids {
                    p = self.emit(Op::LoadPtr(p, Self::off(*k)), Rep::Ptr);
                }
                let v = self.emit(Op::LoadWord(p, Self::off(path.cell)), Rep::Word);
                return Ok(self.guard_lazy(act, st, v, key, pc));
            }
        }
        if self.log {
            std::eprintln!("global {} slow (feedback {})", act.f.name(name), act.f.global(slot).is_some());
        }
        self.flush(st);
        let fs = self.state(act, st, pc);
        let v = self.emit(Op::Call(Helper::LoadGlobal(act.id, u32::try_from(name).unwrap_or(0), u32::try_from(slot).unwrap_or(0), typeof_), vec![], Some(fs)), Rep::Word);
        Self::forget(st);
        Ok(v)
    }

    fn object_literal(&mut self, act: &Act<'e>, st: &mut St, k: OperandValue, pc: usize) -> R<V> {
        let Some(Constant::Object(b)) = act.f.constant(k) else { return Err(Reject::Unsupported) };
        let shape = act.f.literal_shape(k);
        let consts: Option<Vec<u64>> = b.properties.iter().map(|(_, v)| match v {
            None => Some(ValueBits::UNDEFINED),
            Some(LiteralValue::Number(x)) => Some(if x.is_nan() { ValueBits::CANONICAL_NAN } else { x.to_bits() }),
            Some(LiteralValue::Bool(b)) => Some(if *b { ValueBits::TRUE } else { ValueBits::FALSE }),
            Some(LiteralValue::Null) => Some(ValueBits::NULL),
            Some(LiteralValue::Undefined) => Some(ValueBits::UNDEFINED),
            _ => None,
        }).collect();
        let _ = pc;
        if let Some(consts) = consts
            && (b.properties.is_empty() || shape.is_some())
        {
            self.keep(&shape);
            let proto = self.emit(Op::CtxField(Ctx::OBJECT_PROTO), Rep::Ptr);
            let fields = consts.into_iter().map(|w| self.word(w)).collect();
            let id = self.emit(Op::Word(0), Rep::Ptr);
            st.heap.objs.push((id, VObj { proto, shape: if b.properties.is_empty() { None } else { shape }, fields }));
            return Ok(id);
        }
        Ok(self.emit(Op::Call(Helper::ObjectLiteral(act.id, u32::try_from(k).unwrap_or(0)), vec![], None), Rep::Word))
    }
}

enum Flow {
    Next,
    Jump(usize),
    Branch(V, usize, bool),
    Loop(usize),
    End,
}
