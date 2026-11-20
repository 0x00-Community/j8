//! Graph ⇒ x86-64 machine code directly: linear-scan register allocation over the blocks in reverse postorder, then
//! one pass of instruction selection. Guards jump to cold stubs that store the frame state's values into
//! `Ctx::BUF` and call `j8_deopt`.
//!
//! ```text
//! rbp frame: saved rbx r12 r13 r14 r15 at −8 … −40, entry pointers at −48 … −88, spill slots below
//! r15        the context (Ctx) the whole time
//! scratch    rax rcx rdx r11 xmm14 xmm15 (never allocated)
//! allocated  rsi rdi r8 r9 r10 (caller-saved), rbx r12 r13 r14 (callee-saved), xmm0 … xmm13
//! calls      operands through Ctx::ARGS, then the argument registers; values live across a call sit in callee-saved
//!            registers or stack slots; slow paths that call (poll, write barrier) save the caller-saved ones
//! ```
use alloc::collections::BTreeMap;
use alloc::vec;
use alloc::vec::Vec;

use j8executor::FrameSlots;
use j8framework::{Layout, ValueBits};

use crate::asm::{Alu, Asm, Cc, Label, R, RAX, RBP, RBX, RCX, RDI, RDX, RSI, RSP, R10, R11, R12, R13, R14, R15, R8, R9, X};
use crate::build::{Kind, OBJECT_TAG};
use crate::ir::{Cond, FloatOp, Graph, Helper, IntOp, Op, Rep, Slot, B, Fs, V};
use crate::lower::{Exit, Reason, RETURNED};
use crate::runtime::Ctx;

/// Where a value lives.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Loc {
    Gpr(R),
    Xmm(X),
    /// `[rbp + d]`
    Stack(i32),
    /// Rematerialized at each use
    Const,
    None,
}

/// What the code calls (patched by the installer).
#[derive(Clone, Copy, Debug)]
pub enum Sym {
    Deopt,
    Poll,
    Helper,
    Target(u32),
}

/// The machine code and what the runtime needs to read it.
pub struct Generated {
    pub bytes: Vec<u8>,
    pub relocs: Vec<(usize, Sym)>,
    pub exits: Vec<Exit>,
    pub values: Vec<Vec<(V, u32)>>,
    pub calls: Vec<Helper>,
    pub arity: Vec<usize>,
}

const CALLER_SAVED: [R; 5] = [RSI, RDI, R8, R9, R10];
const CALLEE_SAVED: [R; 4] = [RBX, R12, R13, R14];
const XMMS: u8 = 14;
const XS: X = 15;
const XT: X = 14;
/// Saved registers end here; entry pointers below
const SAVED_END: i32 = -40;
/// Entry pointer slots: interpreter params / r0, or native callee / this / a0 / a1 / a2
const ENTRY_SLOT: i32 = -48;

struct Interval {
    v: V,
    lo: u32,
    hi: u32,
    float: bool,
    across: bool,
}

pub struct Gen<'g> {
    g: &'g Graph,
    /// Global cache sites' addresses
    globals: &'g [u64],
    kind: Kind,
    a: Asm,
    loc: Vec<Loc>,
    frame: i32,
    blocks: BTreeMap<B, Label>,
    relocs: Vec<(usize, Sym)>,
    exits: Vec<Exit>,
    values: Vec<Vec<(V, u32)>>,
    calls: Vec<Helper>,
    arity: Vec<usize>,
    deopt_base: u32,
    helper_base: u32,
    /// (fs, reason) ⇒ its stub's label
    stubs: Vec<(Fs, Reason, Label)>,
    /// Cold code emitted after the body: (label, what)
    cold: Vec<(Label, Cold)>,
    /// Uses of each value (to fuse compares into their only branch)
    uses: Vec<u32>,
    /// Caller-saved registers in use anywhere (slow paths save them)
    used_caller: Vec<R>,
    used_xmm: u8,
}

/// Out-of-line code.
enum Cold {
    /// Poll: call j8_poll, back to `cont` or exit
    Poll { cont: Label, exit: Label },
    /// Write barrier: helper StoreCell(addr in rcx, word in rdx), back to `cont`
    Barrier { cont: Label, id: u32 },
    /// Global cache miss: helper GlobalProbe(cache), the value in rax back at `cont`
    Global { cont: Label, id: u32, cache: u64, miss: Label, throw: Label },
}

impl<'g> Gen<'g> {
    pub fn generate(g: &'g Graph, kind: Kind, deopt_base: u32, helper_base: u32, globals: &'g [u64]) -> Generated {
        let mut cg = Gen { g, kind, globals, a: Asm::default(), loc: vec![Loc::None; g.insts.len()], frame: 0, blocks: BTreeMap::new(), relocs: Vec::new(), exits: Vec::new(), values: Vec::new(), calls: Vec::new(), arity: Vec::new(), deopt_base, helper_base, stubs: Vec::new(), cold: Vec::new(), uses: vec![0; g.insts.len()], used_caller: Vec::new(), used_xmm: 0 };
        let order = cg.order();
        cg.allocate(&order);
        cg.emit(&order);
        let _ = cg.a.finish();
        Generated { bytes: cg.a.bytes, relocs: cg.relocs, exits: cg.exits, values: cg.values, calls: cg.calls, arity: cg.arity }
    }

    /// Reachable blocks in reverse postorder
    fn order(&self) -> Vec<B> {
        let n = self.g.blocks.len();
        let mut seen = vec![false; n];
        let mut post = Vec::new();
        let mut stack: Vec<(B, usize)> = vec![(0, 0)];
        seen[0] = true;
        while let Some((b, i)) = stack.pop() {
            let succ = self.g.succs(b);
            if i < succ.len() {
                stack.push((b, i + 1));
                let s = succ[i];
                if !seen[s as usize] {
                    seen[s as usize] = true;
                    stack.push((s, 0));
                }
            } else {
                post.push(b);
            }
        }
        post.reverse();
        // Cold blocks last
        let (hot, cold): (Vec<B>, Vec<B>) = post.into_iter().partition(|b| !self.g.blocks[*b as usize].cold);
        hot.into_iter().chain(cold).collect()
    }

    fn is_const(&self, v: V) -> bool {
        matches!(self.g.op(v), Op::I32(_) | Op::F64(_) | Op::Word(_))
    }

    /// Values an instruction reads (operands and its frame state's values)
    fn reads(&self, v: V, out: &mut Vec<V>) {
        let op = self.g.op(v);
        op.inputs(out);
        if let Op::Phi(_) = op {
            out.clear();
        }
        if let Some(fs) = op.frame_state() {
            Self::state_values(self.g, fs, out);
        }
    }

    fn state_values(g: &Graph, fs: Fs, out: &mut Vec<V>) {
        let st = &g.states[fs as usize];
        let mut add = |s: &Slot, out: &mut Vec<V>| {
            if let Slot::Value(v) = s {
                out.push(*v);
            }
        };
        for fr in &st.frames {
            add(&fr.callee, out);
            fr.params.iter().for_each(|s| add(s, out));
            fr.regs.iter().for_each(|s| add(s, out));
            add(&fr.context, out);
            add(&fr.acc, out);
            if let Some((a, b)) = &fr.construct {
                add(a, out);
                add(b, out);
            }
        }
        for o in &st.objects {
            out.push(o.proto);
            o.fields.iter().for_each(|s| add(s, out));
        }
        for (c, _, s) in &st.writes {
            out.push(*c);
            add(s, out);
        }
    }

    fn calls_out(op: &Op) -> bool {
        matches!(op, Op::Call(..) | Op::CallJit(..))
    }

    // ---- allocation ----

    fn allocate(&mut self, order: &[B]) {
        let g = self.g;
        let n = g.insts.len();
        let mut pos = vec![u32::MAX; n];
        let mut start = vec![0u32; g.blocks.len()];
        let mut end = vec![0u32; g.blocks.len()];
        let mut p = 0u32;
        let mut calls = Vec::new();
        for &b in order {
            start[b as usize] = p;
            for &v in &g.blocks[b as usize].insts {
                p += 2;
                pos[v as usize] = p;
                if Self::calls_out(g.op(v)) {
                    calls.push(p);
                }
            }
            p += 2;
            end[b as usize] = p;
        }
        // Block liveness (phi inputs live out of their predecessor)
        let words = n.div_ceil(64);
        let mut gen_ = vec![vec![0u64; words]; g.blocks.len()];
        let mut kill = vec![vec![0u64; words]; g.blocks.len()];
        let mut phi_out: Vec<Vec<V>> = vec![Vec::new(); g.blocks.len()];
        let set = |b: &mut Vec<u64>, v: V| b[v as usize / 64] |= 1 << (v % 64);
        let has = |b: &Vec<u64>, v: V| b[v as usize / 64] >> (v % 64) & 1 == 1;
        let mut ins = Vec::new();
        for &b in order {
            let bi = b as usize;
            for &v in &g.blocks[bi].insts {
                if let Op::Phi(inputs) = g.op(v) {
                    for (k, &x) in inputs.iter().enumerate() {
                        if let Some(&pb) = g.blocks[bi].preds.get(k) {
                            phi_out[pb as usize].push(x);
                        }
                    }
                    set(&mut kill[bi], v);
                    continue;
                }
                self.reads(v, &mut ins);
                for &x in &ins {
                    self.uses[x as usize] += 1;
                    if !has(&kill[bi], x) {
                        set(&mut gen_[bi], x);
                    }
                }
                set(&mut kill[bi], v);
            }
        }
        for &b in order {
            for &x in &phi_out[b as usize].clone() {
                self.uses[x as usize] += 1;
            }
        }
        let mut live_in = vec![vec![0u64; words]; g.blocks.len()];
        let mut live_out = vec![vec![0u64; words]; g.blocks.len()];
        loop {
            let mut changed = false;
            for &b in order.iter().rev() {
                let bi = b as usize;
                let mut out = vec![0u64; words];
                for s in g.succs(b) {
                    for (o, i) in out.iter_mut().zip(&live_in[s as usize]) {
                        *o |= *i;
                    }
                    // A successor's phis are defined there, not live into it from here
                }
                for &x in &phi_out[bi] {
                    set(&mut out, x);
                }
                let mut inn = gen_[bi].clone();
                for ((i, o), k) in inn.iter_mut().zip(&out).zip(&kill[bi]) {
                    *i |= o & !k;
                }
                // Phis of this block are defined at its start: not live in
                for &v in &g.blocks[bi].insts {
                    if matches!(g.op(v), Op::Phi(_)) {
                        inn[v as usize / 64] &= !(1 << (v % 64));
                    }
                }
                if out != live_out[bi] || inn != live_in[bi] {
                    live_out[bi] = out;
                    live_in[bi] = inn;
                    changed = true;
                }
            }
            if !changed {
                break;
            }
        }
        // Intervals
        let mut lo = vec![u32::MAX; n];
        let mut hi = vec![0u32; n];
        for &b in order {
            let bi = b as usize;
            for &v in &g.blocks[bi].insts {
                let at = if matches!(g.op(v), Op::Phi(_)) { start[bi] } else { pos[v as usize] };
                lo[v as usize] = lo[v as usize].min(at);
                hi[v as usize] = hi[v as usize].max(at);
                if !matches!(g.op(v), Op::Phi(_)) {
                    let op = g.op(v);
                    op.inputs(&mut ins);
                    for &x in &ins {
                        hi[x as usize] = hi[x as usize].max(pos[v as usize]);
                    }
                    // A call's frame state is read after it returns (a pending exception)
                    if let Some(fs) = op.frame_state() {
                        ins.clear();
                        Self::state_values(g, fs, &mut ins);
                        let at = pos[v as usize] + u32::from(Self::calls_out(op));
                        for &x in &ins {
                            hi[x as usize] = hi[x as usize].max(at);
                        }
                    }
                }
            }
            for &x in &phi_out[bi] {
                hi[x as usize] = hi[x as usize].max(end[bi]);
            }
            for x in 0..n {
                let xv = u32::try_from(x).unwrap_or(0);
                if has(&live_in[bi], xv) {
                    lo[x] = lo[x].min(start[bi]);
                    hi[x] = hi[x].max(start[bi]);
                }
                if has(&live_out[bi], xv) {
                    hi[x] = hi[x].max(end[bi]);
                }
            }
        }
        let mut ivs: Vec<Interval> = Vec::new();
        for v in 0..n {
            let vv = u32::try_from(v).unwrap_or(0);
            if lo[v] == u32::MAX || g.insts[v].rep == Rep::None {
                continue;
            }
            if self.is_const(vv) {
                self.loc[v] = Loc::Const;
                continue;
            }
            let (l, h) = (lo[v], hi[v].max(lo[v]));
            let across = calls.iter().any(|&c| l < c && c < h);
            ivs.push(Interval { v: vv, lo: l, hi: h, float: g.insts[v].rep == Rep::F64, across });
        }
        ivs.sort_by_key(|i| (i.lo, i.hi));
        let mut active: Vec<(u32, Loc)> = Vec::new();
        let mut slots = 0i32;
        let mut free_slots: Vec<(u32, i32)> = Vec::new();
        for iv in &ivs {
            active.retain(|&(h, _)| h > iv.lo);
            free_slots.iter_mut().for_each(|_| {});
            let busy = |l: Loc, active: &Vec<(u32, Loc)>| active.iter().any(|&(_, a)| a == l);
            let pick = if iv.float {
                if iv.across { None } else { (0..XMMS).map(Loc::Xmm).find(|&l| !busy(l, &active)) }
            } else {
                let pool: Vec<R> = if iv.across { CALLEE_SAVED.to_vec() } else { CALLER_SAVED.iter().chain(&CALLEE_SAVED).copied().collect() };
                pool.into_iter().map(Loc::Gpr).find(|&l| !busy(l, &active))
            };
            let l = match pick {
                Some(l) => l,
                None => {
                    // A stack slot no live value uses
                    match free_slots.iter().position(|&(h, _)| h <= iv.lo) {
                        Some(k) => {
                            let (_, d) = free_slots.remove(k);
                            Loc::Stack(d)
                        }
                        None => {
                            slots += 1;
                            Loc::Stack(ENTRY_SLOT - 8 * 5 - 8 * slots)
                        }
                    }
                }
            };
            match l {
                Loc::Gpr(r) if CALLER_SAVED.contains(&r) && !self.used_caller.contains(&r) => self.used_caller.push(r),
                Loc::Xmm(x) => self.used_xmm = self.used_xmm.max(x + 1),
                _ => {}
            }
            if let Loc::Stack(d) = l {
                free_slots.push((iv.hi, d));
            } else {
                active.push((iv.hi, l));
            }
            self.loc[iv.v as usize] = l;
        }
        // Frame: entry slots (5) + spill slots + room to save xmm registers in slow paths, 16-aligned at calls
        let size = 8 * 5 + 8 * slots + 8 * i32::from(XMMS);
        // After push rbp + 5 pushes, rsp ≡ 8 (mod 16): the frame size must be ≡ 8 too
        self.frame = (size + 15) / 16 * 16 + 8;
    }

    /// Where slow paths save xmm registers
    fn xmm_save(&self, x: X) -> i32 {
        SAVED_END - self.frame + 8 * i32::from(x)
    }

    // ---- moving values ----

    fn loc(&self, v: V) -> Loc {
        self.loc[v as usize]
    }

    fn const_bits(&self, v: V) -> Option<u64> {
        match self.g.op(v) {
            Op::I32(x) => Some(u64::from(x.cast_unsigned())),
            Op::F64(x) => Some(x.to_bits()),
            Op::Word(w) => Some(*w),
            _ => None,
        }
    }

    /// `v` in a general register: its own, or loaded into `scratch`
    fn gpr(&mut self, v: V, scratch: R) -> R {
        match self.loc(v) {
            Loc::Gpr(r) => r,
            Loc::Stack(d) => {
                self.a.load(scratch, RBP, d);
                scratch
            }
            Loc::Const => {
                let b = self.const_bits(v).unwrap_or(0);
                let b = if self.g.rep(v) == Rep::I32 { b & 0xFFFF_FFFF } else { b };
                self.a.mov_ri(scratch, b);
                scratch
            }
            Loc::Xmm(x) => {
                self.a.movq_rx(scratch, x);
                scratch
            }
            Loc::None => {
                self.a.mov_ri(scratch, ValueBits::UNDEFINED);
                scratch
            }
        }
    }

    /// `v` into exactly `dst`
    fn into_gpr(&mut self, v: V, dst: R) {
        let r = self.gpr(v, dst);
        self.a.mov_rr(dst, r);
    }

    fn xmm(&mut self, v: V, scratch: X) -> X {
        match self.loc(v) {
            Loc::Xmm(x) => x,
            Loc::Stack(d) => {
                self.a.movsd_load(scratch, RBP, d);
                scratch
            }
            _ => {
                let r = self.gpr(v, RAX);
                self.a.movq_xr(scratch, r);
                scratch
            }
        }
    }

    /// Where a result goes: its register, or a scratch to store from
    fn dst(&self, v: V, scratch: R) -> R {
        match self.loc(v) {
            Loc::Gpr(r) => r,
            _ => scratch,
        }
    }

    /// After computing `v` into `r`: store it if it lives on the stack
    fn put(&mut self, v: V, r: R) {
        match self.loc(v) {
            Loc::Gpr(d) => self.a.mov_rr(d, r),
            Loc::Stack(d) => self.a.store(RBP, d, r),
            Loc::Xmm(x) => self.a.movq_xr(x, r),
            _ => {}
        }
    }

    fn xdst(&self, v: V) -> X {
        match self.loc(v) {
            Loc::Xmm(x) => x,
            _ => XS,
        }
    }

    fn xput(&mut self, v: V, x: X) {
        match self.loc(v) {
            Loc::Xmm(d) => self.a.movsd_xx(d, x),
            Loc::Stack(d) => self.a.movsd_store(RBP, d, x),
            Loc::Gpr(r) => self.a.movq_rx(r, x),
            _ => {}
        }
    }

    // ---- exits ----

    /// The stub handing over with `fs` (one per frame state and reason)
    fn exit(&mut self, fs: Fs, reason: Reason) -> Label {
        if let Some(&(_, _, l)) = self.stubs.iter().find(|(f, r, _)| *f == fs && *r == reason) {
            return l;
        }
        let l = self.a.label();
        self.stubs.push((fs, reason, l));
        l
    }

    fn emit_stub(&mut self, fs: Fs, reason: Reason, l: Label) {
        let id = u32::try_from(self.exits.len()).unwrap_or(0);
        self.exits.push(Exit { fs, reason });
        self.a.bind(l);
        let mut vals = Vec::new();
        Self::state_values(self.g, fs, &mut vals);
        let mut map: Vec<(V, u32)> = Vec::new();
        for v in vals {
            if map.iter().any(|(x, _)| *x == v) || matches!(self.loc(v), Loc::Const | Loc::None) {
                continue;
            }
            let i = u32::try_from(map.len()).unwrap_or(0);
            let disp = Ctx::BUF + 8 * i32::try_from(i).unwrap_or(0);
            match (self.loc(v), self.g.rep(v)) {
                (Loc::Xmm(x), _) => self.a.movsd_store(R15, disp, x),
                (_, Rep::I32) => {
                    let r = self.gpr(v, RAX);
                    self.a.movsxd(RAX, r);
                    self.a.store(R15, disp, RAX);
                }
                _ => {
                    let r = self.gpr(v, RAX);
                    self.a.store(R15, disp, r);
                }
            }
            map.push((v, i));
        }
        self.values.push(map);
        self.a.mov_rr(RDI, R15);
        self.a.mov_ri(RSI, u64::from(self.deopt_base + id));
        let at = self.a.call_abs(0);
        self.relocs.push((at, Sym::Deopt));
        self.epilogue();
    }

    /// Jumps to the exit when `cc` holds
    fn exit_if(&mut self, cc: Cc, fs: Fs, reason: Reason) {
        let l = self.exit(fs, reason);
        self.a.jcc(cc, l);
    }

    // ---- prologue / epilogue ----

    fn prologue(&mut self) {
        self.a.push(RBP);
        self.a.mov_rr(RBP, RSP);
        for r in [RBX, R12, R13, R14, R15] {
            self.a.push(r);
        }
        self.a.alu_ri(Alu::Sub, true, RSP, self.frame);
        self.a.mov_rr(R15, RDI);
        match self.kind {
            Kind::Native(_) => {
                for (i, r) in [RSI, RDX, RCX, R8, R9].into_iter().enumerate() {
                    self.a.store(RBP, ENTRY_SLOT - 8 * i32::try_from(i).unwrap_or(0), r);
                }
            }
            _ => {
                self.a.store(RBP, ENTRY_SLOT, RSI);
                self.a.store(RBP, ENTRY_SLOT - 8, RDX);
            }
        }
    }

    /// Returns rax (natives count their depth down first)
    fn epilogue(&mut self) {
        if let Kind::Native(_) = self.kind {
            self.a.load(RCX, R15, Ctx::DEPTH);
            self.a.alu_ri(Alu::Sub, true, RCX, 1);
            self.a.store(R15, Ctx::DEPTH, RCX);
        }
        self.a.lea(RSP, RBP, SAVED_END);
        for r in [R15, R14, R13, R12, RBX] {
            self.a.pop(r);
        }
        self.a.pop(RBP);
        self.a.ret();
    }

    // ---- the body ----

    fn emit(&mut self, order: &[B]) {
        // Blocks that only jump on are not emitted: jumps to them go to their targets
        let order: Vec<B> = order.iter().copied().filter(|&b| b == 0 || self.through(b) == b).collect();
        for &b in &order {
            let l = self.a.label();
            self.blocks.insert(b, l);
        }
        self.prologue();
        for (i, &b) in order.iter().enumerate() {
            let l = self.blocks[&b];
            self.a.bind(l);
            let next = order.get(i + 1).copied();
            let insts = self.g.blocks[b as usize].insts.clone();
            let next = next;
            for (k, &v) in insts.iter().enumerate() {
                if matches!(self.g.op(v), Op::Phi(_)) {
                    continue;
                }
                // A compare used only by the branch right after it: fused there
                if let Some(&nx) = insts.get(k + 1)
                    && self.fusable(v, nx)
                {
                    continue;
                }
                let prev = if k > 0 { Some(insts[k - 1]) } else { None };
                self.inst(b, v, next, prev);
            }
        }
        let cold = core::mem::take(&mut self.cold);
        for (l, c) in cold {
            self.a.bind(l);
            match c {
                Cold::Poll { cont, exit } => {
                    self.save_caller();
                    self.a.mov_rr(RDI, R15);
                    let at = self.a.call_abs(0);
                    self.relocs.push((at, Sym::Poll));
                    self.restore_caller();
                    self.a.test_rr(true, RAX, RAX);
                    self.a.jcc(Cc::Ne, exit);
                    self.a.jmp(cont);
                }
                Cold::Global { cont, id, cache, miss, throw } => {
                    self.save_caller();
                    self.a.mov_rr(RDI, R15);
                    self.a.mov_ri(RSI, u64::from(id));
                    self.a.mov_ri(RDX, cache);
                    self.a.mov_ri(RCX, 0);
                    self.a.mov_ri(R8, 0);
                    self.a.mov_ri(R9, 0);
                    let at = self.a.call_abs(0);
                    self.relocs.push((at, Sym::Helper));
                    self.restore_caller();
                    self.a.cmp_mi(true, R15, Ctx::MISS, 0);
                    self.a.jcc(Cc::Ne, miss);
                    self.a.cmp_mi(true, R15, Ctx::PENDING, 0);
                    self.a.jcc(Cc::Ne, throw);
                    self.a.jmp(cont);
                }
                Cold::Barrier { cont, id } => {
                    self.save_caller();
                    self.a.mov_rr(RDI, R15);
                    self.a.mov_ri(RSI, u64::from(id));
                    self.a.mov_rr(R8, RDX);
                    self.a.mov_rr(RDX, RCX);
                    self.a.mov_rr(RCX, R8);
                    self.a.mov_ri(R8, 0);
                    self.a.mov_ri(R9, 0);
                    let at = self.a.call_abs(0);
                    self.relocs.push((at, Sym::Helper));
                    self.restore_caller();
                    self.a.jmp(cont);
                }
            }
        }
        let stubs = self.stubs.clone();
        for (fs, reason, l) in stubs {
            self.emit_stub(fs, reason, l);
        }
    }

    fn save_caller(&mut self) {
        let regs = self.used_caller.clone();
        for r in &regs {
            self.a.push(*r);
        }
        if regs.len() % 2 == 1 {
            self.a.alu_ri(Alu::Sub, true, RSP, 8);
        }
        for x in 0..self.used_xmm {
            let d = self.xmm_save(x);
            self.a.movsd_store(RBP, d, x);
        }
    }

    fn restore_caller(&mut self) {
        for x in 0..self.used_xmm {
            let d = self.xmm_save(x);
            self.a.movsd_load(x, RBP, d);
        }
        let regs = self.used_caller.clone();
        if regs.len() % 2 == 1 {
            self.a.alu_ri(Alu::Add, true, RSP, 8);
        }
        for r in regs.iter().rev() {
            self.a.pop(*r);
        }
    }

    /// `v` is a compare whose only use is the branch / check `next`
    fn fusable(&self, v: V, next: V) -> bool {
        matches!(self.g.op(v), Op::CmpInt(..) | Op::CmpWord(..) | Op::CmpFloat(..))
            && self.uses[v as usize] == 1
            && matches!(self.g.op(next), Op::Branch(c, _, _) | Op::Check(c, _, _) if *c == v)
    }

    /// Sets flags for compare `v`; the condition that holds when it is true
    fn compare(&mut self, v: V) -> Cc {
        match self.g.op(v).clone() {
            Op::CmpInt(c, a, b) => {
                let w = self.g.rep(a) != Rep::I32 && self.g.rep(a) != Rep::Bool;
                let ra = self.gpr(a, RAX);
                match self.const_bits(b).and_then(|x| i32::try_from(if w { x.cast_signed() } else { i64::from((x as u32).cast_signed()) }).ok()) {
                    Some(imm) => self.a.alu_ri(Alu::Cmp, w, ra, imm),
                    None => {
                        let rb = self.gpr(b, RCX);
                        self.a.alu_rr(Alu::Cmp, w, ra, rb);
                    }
                }
                Self::icc(c)
            }
            Op::CmpWord(c, a, b) => {
                let ra = self.gpr(a, RAX);
                match self.const_bits(b).and_then(|x| i32::try_from(x.cast_signed()).ok()) {
                    Some(imm) => self.a.alu_ri(Alu::Cmp, true, ra, imm),
                    None => {
                        let rb = self.gpr(b, RCX);
                        self.a.alu_rr(Alu::Cmp, true, ra, rb);
                    }
                }
                Self::icc(c)
            }
            Op::CmpFloat(c, a, b) => {
                let (x, y) = match c {
                    Cond::Lt | Cond::Le => (b, a),
                    _ => (a, b),
                };
                let xa = self.xmm(x, XS);
                let xb = self.xmm(y, XT);
                self.a.ucomisd(xa, xb);
                match c {
                    Cond::Lt | Cond::Gt => Cc::A,
                    Cond::Le | Cond::Ge => Cc::Ae,
                    // Ordered equality needs PF clear too: handled by the caller through `float_eq`
                    Cond::Eq => Cc::E,
                    Cond::Ne => Cc::Ne,
                }
            }
            _ => unreachable!("a compare"),
        }
    }

    fn icc(c: Cond) -> Cc {
        match c {
            Cond::Lt => Cc::L,
            Cond::Le => Cc::Le,
            Cond::Gt => Cc::G,
            Cond::Ge => Cc::Ge,
            Cond::Eq => Cc::E,
            Cond::Ne => Cc::Ne,
        }
    }

    /// A float equality (ordered): ZF set and PF clear
    fn is_float_eq(&self, v: V) -> bool {
        matches!(self.g.op(v), Op::CmpFloat(Cond::Eq, ..))
    }

    /// Jumps to `to` when `cond` (a flag value or a fused compare) is `when`, else falls through
    fn branch_on(&mut self, cond: V, to: Label, fused: bool, when: bool) {
        if fused {
            let feq = self.is_float_eq(cond);
            let cc = self.compare(cond);
            if feq {
                // Unordered: not equal
                if when {
                    let skip = self.a.label();
                    self.a.jcc(Cc::P, skip);
                    self.a.jcc(cc, to);
                    self.a.bind(skip);
                } else {
                    self.a.jcc(Cc::P, to);
                    self.a.jcc(cc.negate(), to);
                }
                return;
            }
            self.a.jcc(if when { cc } else { cc.negate() }, to);
            return;
        }
        let r = self.gpr(cond, RAX);
        self.a.test_rr(false, r, r);
        self.a.jcc(if when { Cc::Ne } else { Cc::E }, to);
    }

    /// A block that only jumps on (no phi moves): its target instead, transitively
    fn through(&self, mut b: B) -> B {
        for _ in 0..8 {
            let blk = &self.g.blocks[b as usize];
            let [only] = blk.insts[..] else { return b };
            let Op::Jump(t) = self.g.op(only) else { return b };
            let has_phis = self.g.blocks[*t as usize].insts.iter().any(|&v| matches!(self.g.op(v), Op::Phi(_)));
            if has_phis {
                return b;
            }
            b = *t;
        }
        b
    }

    fn inst(&mut self, b: B, v: V, next: Option<B>, prev: Option<V>) {
        let op = self.g.op(v).clone();
        let fused_prev = prev.is_some_and(|p| self.fusable(p, v));
        match op {
            Op::Phi(_) | Op::I32(_) | Op::F64(_) | Op::Word(_) => {}
            Op::Param(i) => {
                let d = self.dst(v, RAX);
                match self.kind {
                    Kind::Native(_) => self.a.load(d, RBP, ENTRY_SLOT - 8 * (1 + i32::try_from(i.min(3)).unwrap_or(0))),
                    _ => {
                        let (p, r) = (self.g.entry_params, self.g.entry_regs);
                        let i = i as usize;
                        let (slot, off) = if i < p {
                            (ENTRY_SLOT, 8 * i)
                        } else if i < p + r {
                            (ENTRY_SLOT - 8, 8 * (i - p))
                        } else {
                            let k = i - p - r;
                            let below = |s: usize| 0usize.wrapping_sub(8 * s);
                            match k {
                                0 => (ENTRY_SLOT - 8, below(FrameSlots::CONTEXT)),
                                1 => (ENTRY_SLOT - 8, below(FrameSlots::CLOSURE)),
                                2 => {
                                    self.a.load(d, R15, Ctx::ACC);
                                    self.put(v, d);
                                    return;
                                }
                                3 => (ENTRY_SLOT - 8, below(FrameSlots::NEW_TARGET)),
                                _ => (ENTRY_SLOT - 8, below(FrameSlots::NEW_THIS)),
                            }
                        };
                        self.a.load(RCX, RBP, slot);
                        self.a.load(d, RCX, i32::try_from(off.cast_signed()).unwrap_or(0));
                    }
                }
                self.put(v, d);
            }
            Op::Callee => {
                let d = self.dst(v, RAX);
                self.a.load(d, RBP, ENTRY_SLOT);
                self.put(v, d);
            }
            Op::CtxField(off) => {
                let d = self.dst(v, RAX);
                self.a.load(d, R15, off);
                self.put(v, d);
            }
            Op::Int(op, x, y, fs) => self.int(v, op, x, y, fs),
            Op::Float(op, x, y) => {
                let code = match op {
                    FloatOp::Add => 0x58,
                    FloatOp::Mul => 0x59,
                    FloatOp::Sub => 0x5c,
                    FloatOp::Div => 0x5e,
                };
                let xa = self.xmm(x, XS);
                self.a.movsd_xx(XS, xa);
                let xb = self.xmm(y, XT);
                self.a.sse(code, XS, xb);
                self.xput(v, XS);
            }
            Op::IntToFloat(x) => {
                let r = self.gpr(x, RAX);
                let d = self.xdst(v);
                self.a.zero_x(d);
                self.a.cvtsi2sd(d, r);
                self.xput(v, d);
            }
            Op::BoxInt(x) => {
                let r = self.gpr(x, RAX);
                self.a.zero_x(XS);
                self.a.cvtsi2sd(XS, r);
                let d = self.dst(v, RAX);
                self.a.movq_rx(d, XS);
                self.put(v, d);
            }
            Op::BoxFloat(x) => {
                let xa = self.xmm(x, XS);
                let d = self.dst(v, RAX);
                self.a.movq_rx(RAX, xa);
                self.a.mov_ri(RCX, ValueBits::CANONICAL_NAN);
                self.a.ucomisd(xa, xa);
                self.a.cmov(Cc::P, RAX, RCX);
                self.a.mov_rr(d, RAX);
                self.put(v, d);
            }
            Op::BoxBool(x) => {
                let r = self.gpr(x, RCX);
                self.a.mov_ri(RAX, ValueBits::FALSE);
                self.a.alu_rr(Alu::Add, true, RAX, r);
                self.put(v, RAX);
            }
            Op::BoxPtr(x, tag) => {
                let r = self.gpr(x, RAX);
                self.a.mov_ri(RCX, tag);
                self.a.alu_rr(Alu::Or, true, RCX, r);
                self.put(v, RCX);
            }
            Op::Untag(x) => {
                let r = self.gpr(x, RAX);
                let d = self.dst(v, RAX);
                self.a.mov_rr(d, r);
                self.untag(d);
                self.put(v, d);
            }
            Op::UnboxInt(x, fs) => {
                let r = self.gpr(x, RAX);
                self.a.mov_rr(RAX, r);
                self.a.mov_ri(RCX, ValueBits::BOXED_MIN);
                self.a.alu_rr(Alu::Cmp, true, RAX, RCX);
                self.exit_if(Cc::Ae, fs, Reason::Number);
                self.a.movq_xr(XS, RAX);
                self.to_int(v, fs);
            }
            Op::FloatToInt(x, fs) => {
                let xa = self.xmm(x, XS);
                self.a.movsd_xx(XS, xa);
                self.a.movq_rx(RAX, XS);
                self.to_int(v, fs);
            }
            Op::UnboxFloat(x, fs) => {
                let r = self.gpr(x, RAX);
                self.a.mov_rr(RAX, r);
                self.a.mov_ri(RCX, ValueBits::BOXED_MIN);
                self.a.alu_rr(Alu::Cmp, true, RAX, RCX);
                self.exit_if(Cc::Ae, fs, Reason::Number);
                let d = self.xdst(v);
                self.a.movq_xr(d, RAX);
                self.xput(v, d);
            }
            Op::CheckObject(x, fs) => {
                let r = self.gpr(x, RAX);
                self.a.mov_rr(RAX, r);
                self.a.mov_rr(RCX, r);
                self.a.shift_ri(5, true, RCX, 48);
                self.a.alu_ri(Alu::Cmp, false, RCX, i32::try_from(OBJECT_TAG >> 48).unwrap_or(0));
                self.exit_if(Cc::Ne, fs, Reason::Shape);
                self.untag(RAX);
                self.put(v, RAX);
            }
            Op::CheckNotHole(x, fs) => {
                let r = self.gpr(x, RAX);
                self.a.mov_ri(RCX, ValueBits::UNINIT);
                self.a.alu_rr(Alu::Cmp, true, r, RCX);
                self.exit_if(Cc::E, fs, Reason::Hole);
            }
            Op::CheckFunc(p, f, fs) => {
                let r = self.gpr(p, RAX);
                self.a.load(RCX, r, Self::o(Layout::OBJECT_CALL));
                self.untag(RCX);
                self.exit_if(Cc::E, fs, Reason::Func);
                self.a.cmp_m8i(RCX, 0, Layout::FAST_CLOSURE_TAG);
                self.exit_if(Cc::Ne, fs, Reason::Func);
                self.a.cmp_mi(false, RCX, Self::o(Layout::FAST_CLOSURE + Layout::CLOSURE_FUNC), i32::try_from(f.0).unwrap_or(-1));
                self.exit_if(Cc::Ne, fs, Reason::Func);
            }
            Op::CheckShape(p, shape, fs) => {
                let r = self.gpr(p, RAX);
                let want = shape.as_ref().map_or(0, |s| alloc::sync::Arc::as_ptr(s) as usize as u64);
                self.a.mov_ri(RCX, want);
                self.a.alu_rm(Alu::Cmp, true, RCX, r, Self::o(Layout::OBJECT_SHAPE));
                self.exit_if(Cc::Ne, fs, Reason::Shape);
                self.a.test_m8i(r, Self::o(Layout::OBJECT_CLASS), Layout::OBJECT_PLAIN_BIT);
                self.exit_if(Cc::E, fs, Reason::Shape);
                if shape.is_none() {
                    self.a.cmp_mi(true, r, Self::o(Layout::OBJECT_DICT), 0);
                    self.exit_if(Cc::Ne, fs, Reason::Shape);
                }
            }
            Op::Check(flag, expect, fs) => {
                if fused_prev {
                    let feq = self.is_float_eq(flag);
                    let cc = self.compare(flag);
                    let cc = if expect { cc } else { cc.negate() };
                    if feq && expect {
                        self.exit_if(Cc::P, fs, Reason::Shape);
                    }
                    self.exit_if(cc.negate(), fs, Reason::Shape);
                } else {
                    let r = self.gpr(flag, RAX);
                    self.a.test_rr(false, r, r);
                    self.exit_if(if expect { Cc::E } else { Cc::Ne }, fs, Reason::Shape);
                }
            }
            Op::CmpInt(..) | Op::CmpWord(..) | Op::CmpFloat(..) => {
                let feq = self.is_float_eq(v);
                let cc = self.compare(v);
                let d = self.dst(v, RAX);
                self.a.setcc(cc, d);
                if feq {
                    self.a.setcc(Cc::Np, RCX);
                    self.a.alu_rr(Alu::And, false, d, RCX);
                }
                self.put(v, d);
            }
            Op::LoadWord(base, off) | Op::LoadRaw(base, off) => {
                let r = self.gpr(base, RAX);
                let d = self.dst(v, RAX);
                self.a.load(d, r, off);
                self.put(v, d);
            }
            Op::LoadPtr(base, off) => {
                let r = self.gpr(base, RAX);
                let d = self.dst(v, RAX);
                self.a.load(d, r, off);
                self.untag(d);
                self.put(v, d);
            }
            Op::LoadByte(base, off) => {
                let r = self.gpr(base, RAX);
                let d = self.dst(v, RAX);
                self.a.load8(d, r, off);
                self.put(v, d);
            }
            Op::StoreCell(base, off, w) => {
                let r = self.gpr(base, RAX);
                self.a.lea(RCX, r, off);
                let wr = self.gpr(w, RDX);
                self.a.mov_rr(RDX, wr);
                self.a.load(RAX, RCX, 0);
                self.a.alu_rr(Alu::Or, true, RAX, RDX);
                self.a.mov_ri(R11, j8framework::paragc::WORD_POINTER_MIN);
                self.a.alu_rr(Alu::Cmp, true, RAX, R11);
                let slow = self.a.label();
                let cont = self.a.label();
                self.a.jcc(Cc::Ae, slow);
                self.a.store(RCX, 0, RDX);
                self.a.bind(cont);
                let id = self.helper_id(Helper::StoreCell, 2);
                self.cold.push((slow, Cold::Barrier { cont, id }));
            }
            Op::StoreCellAddr(o, slot) => {
                let r = self.gpr(o, RAX);
                let d = self.dst(v, RAX);
                self.a.load(d, r, Self::o(Layout::OBJECT_STORE));
                self.untag(d);
                self.a.alu_ri(Alu::Add, true, d, Self::o(Layout::STORE_CELLS + (slot as usize - Layout::INLINE_PROPS) * Layout::CELL));
                self.put(v, d);
            }
            Op::ContextCell(c, idx) => {
                let r = self.gpr(c, RAX);
                let k = idx as usize * Layout::CELL;
                self.a.load(RCX, r, Self::o(Layout::ENV_BOXED_PTR));
                self.a.alu_ri(Alu::Add, true, RCX, Self::o(k));
                self.a.lea(RDX, r, Self::o(Layout::ENV_INLINE_CELLS + k));
                self.a.cmp_m8i(r, Self::o(Layout::ENV_TAG), Layout::ENV_INLINE_TAG);
                self.a.cmov(Cc::Ne, RDX, RCX);
                self.put(v, RDX);
            }
            Op::Poll(fs) => {
                self.a.load(RAX, R15, Ctx::BUDGET);
                self.a.alu_ri(Alu::Sub, true, RAX, 1);
                self.a.store(R15, Ctx::BUDGET, RAX);
                let slow = self.a.label();
                let cont = self.a.label();
                self.a.jcc(Cc::Le, slow);
                self.a.bind(cont);
                let exit = self.exit(fs, Reason::Poll);
                self.cold.push((slow, Cold::Poll { cont, exit }));
            }
            Op::Global(k, _, fs) => {
                let cache = self.globals[k as usize];
                self.a.mov_ri(RCX, cache);
                self.a.load(RAX, R15, Ctx::EPOCH);
                self.a.alu_rm(Alu::Cmp, true, RAX, RCX, 0);
                let slow = self.a.label();
                let cont = self.a.label();
                self.a.jcc(Cc::Ne, slow);
                self.a.load(RAX, RCX, 8);
                self.a.bind(cont);
                self.put(v, RAX);
                let id = self.helper_id(Helper::GlobalProbe(k), 1);
                let miss = self.exit(fs, Reason::Func);
                let throw = self.exit(fs, Reason::Throw);
                self.cold.push((slow, Cold::Global { cont, id, cache, miss, throw }));
            }
            Op::Enter(fs) => {
                self.a.load(RAX, R15, Ctx::DEPTH);
                self.a.alu_ri(Alu::Add, true, RAX, 1);
                self.a.store(R15, Ctx::DEPTH, RAX);
                self.a.alu_rm(Alu::Cmp, true, RAX, R15, Ctx::LIMIT);
                self.exit_if(Cc::Ae, fs, Reason::Depth);
            }
            Op::Call(h, args, fs) => {
                let id = self.helper_id(h, args.len());
                self.args_to_ctx(&args);
                self.a.mov_rr(RDI, R15);
                self.a.mov_ri(RSI, u64::from(id));
                if args.len() > 4 {
                    self.a.mov_ri(RDX, args.len() as u64);
                } else {
                    for (i, r) in [RDX, RCX, R8, R9].into_iter().enumerate() {
                        self.a.load(r, R15, Ctx::ARGS + 8 * i32::try_from(i).unwrap_or(0));
                    }
                }
                let at = self.a.call_abs(0);
                self.relocs.push((at, Sym::Helper));
                if let Some(fs) = fs
                    && h.throws()
                {
                    self.a.cmp_mi(true, R15, Ctx::PENDING, 0);
                    self.exit_if(Cc::Ne, fs, Reason::Throw);
                }
                match h.rep() {
                    Rep::None => {}
                    Rep::F64 => {
                        let d = self.xdst(v);
                        self.a.movq_xr(d, RAX);
                        self.xput(v, d);
                    }
                    _ => self.put(v, RAX),
                }
            }
            Op::CallJit(k, callee, this, args, fs) => {
                let mut all = vec![callee, this];
                all.extend_from_slice(&args);
                self.args_to_ctx(&all);
                for i in args.len()..3 {
                    self.a.mov_ri(RAX, ValueBits::UNDEFINED);
                    self.a.store(R15, Ctx::ARGS + 8 * i32::try_from(2 + i).unwrap_or(0), RAX);
                }
                self.a.mov_rr(RDI, R15);
                for (i, r) in [RSI, RDX, RCX, R8, R9].into_iter().enumerate() {
                    self.a.load(r, R15, Ctx::ARGS + 8 * i32::try_from(i).unwrap_or(0));
                }
                let at = self.a.call_abs(0);
                self.relocs.push((at, Sym::Target(k)));
                self.a.cmp_mi(true, R15, Ctx::PENDING, 0);
                self.exit_if(Cc::Ne, fs, Reason::Throw);
                self.put(v, RAX);
            }
            Op::Jump(t) => {
                self.phi_moves(b, t);
                let t = self.through(t);
                if next != Some(t) {
                    let l = self.blocks[&t];
                    self.a.jmp(l);
                }
            }
            Op::Branch(c, t, f) => {
                let (t, f) = (self.through(t), self.through(f));
                let (lt, lf) = (self.blocks[&t], self.blocks[&f]);
                // Fall into the next block when it is one side
                if next == Some(t) {
                    self.branch_on(c, lf, fused_prev, false);
                } else {
                    self.branch_on(c, lt, fused_prev, true);
                    if next != Some(f) {
                        self.a.jmp(lf);
                    }
                }
            }
            Op::Return(w, pc) => {
                match self.kind {
                    Kind::Native(_) => self.into_gpr(w, RAX),
                    _ => {
                        let r = self.gpr(w, RAX);
                        self.a.store(R15, Ctx::RET, r);
                        self.a.mov_ri(RCX, u64::from(pc));
                        self.a.store(R15, Ctx::RET_PC, RCX);
                        self.a.mov_ri(RAX, RETURNED as u64);
                    }
                }
                self.epilogue();
            }
            Op::Deopt(fs) => {
                let l = self.exit(fs, Reason::Always);
                self.a.jmp(l);
            }
        }
    }

    fn o(x: usize) -> i32 {
        i32::try_from(x).unwrap_or(i32::MAX)
    }

    /// Clears the tag: the low 48 bits stay
    fn untag(&mut self, r: R) {
        self.a.shift_ri(4, true, r, 16);
        self.a.shift_ri(5, true, r, 16);
    }

    fn helper_id(&mut self, h: Helper, n: usize) -> u32 {
        let id = self.helper_base + u32::try_from(self.calls.len()).unwrap_or(0);
        self.calls.push(h);
        self.arity.push(n);
        id
    }

    /// Operands as words into Ctx::ARGS (in order)
    fn args_to_ctx(&mut self, args: &[V]) {
        for (i, &x) in args.iter().enumerate() {
            let d = Ctx::ARGS + 8 * i32::try_from(i).unwrap_or(0);
            match (self.loc(x), self.g.rep(x)) {
                (Loc::Xmm(r), _) => self.a.movsd_store(R15, d, r),
                (_, Rep::I32) => {
                    let r = self.gpr(x, RAX);
                    self.a.movsxd(RAX, r);
                    self.a.store(R15, d, RAX);
                }
                _ => {
                    let r = self.gpr(x, RAX);
                    self.a.store(R15, d, r);
                }
            }
        }
    }

    /// The double in XS ⇒ int32 in `v`'s place; RAX holds its bits (for −0)
    fn to_int(&mut self, v: V, fs: Fs) {
        self.a.cvttsd2si(RCX, XS);
        self.a.zero_x(XT);
        self.a.cvtsi2sd(XT, RCX);
        self.a.ucomisd(XS, XT);
        self.exit_if(Cc::Ne, fs, Reason::Int);
        self.exit_if(Cc::P, fs, Reason::Int);
        // −0: an int 0 from nonzero bits
        let ok = self.a.label();
        self.a.test_rr(false, RCX, RCX);
        self.a.jcc(Cc::Ne, ok);
        self.a.test_rr(true, RAX, RAX);
        self.exit_if(Cc::Ne, fs, Reason::Int);
        self.a.bind(ok);
        self.put(v, RCX);
    }

    fn int(&mut self, v: V, op: IntOp, x: V, y: V, fs: Option<Fs>) {
        let imm = self.const_bits(y).map(|b| (b as u32).cast_signed());
        match op {
            IntOp::Add | IntOp::Sub | IntOp::And | IntOp::Or | IntOp::Xor => {
                let alu = match op {
                    IntOp::Add => Alu::Add,
                    IntOp::Sub => Alu::Sub,
                    IntOp::And => Alu::And,
                    IntOp::Or => Alu::Or,
                    _ => Alu::Xor,
                };
                let rx = self.gpr(x, RAX);
                self.a.mov_rr32(RAX, rx);
                match imm {
                    Some(i) => self.a.alu_ri(alu, false, RAX, i),
                    None => {
                        let ry = self.gpr(y, RCX);
                        self.a.alu_rr(alu, false, RAX, ry);
                    }
                }
                if let Some(fs) = fs
                    && matches!(op, IntOp::Add | IntOp::Sub)
                {
                    self.exit_if(Cc::O, fs, Reason::Int);
                }
                self.put(v, RAX);
            }
            IntOp::Mul => {
                let rx = self.gpr(x, RAX);
                self.a.mov_rr32(RAX, rx);
                match imm {
                    Some(i) => self.a.imul_rri(false, RAX, RAX, i),
                    None => {
                        let ry = self.gpr(y, RCX);
                        self.a.imul_rr(false, RAX, ry);
                    }
                }
                if let Some(fs) = fs {
                    self.exit_if(Cc::O, fs, Reason::Int);
                    // 0 from a negative operand is −0
                    let ok = self.a.label();
                    self.a.test_rr(false, RAX, RAX);
                    self.a.jcc(Cc::Ne, ok);
                    let rx = self.gpr(x, RCX);
                    self.a.mov_rr32(RCX, rx);
                    let ry = self.gpr(y, RDX);
                    self.a.alu_rr(Alu::Or, false, RCX, ry);
                    self.exit_if(Cc::L, fs, Reason::Int);
                    self.a.bind(ok);
                }
                self.put(v, RAX);
            }
            IntOp::Mod => {
                let fs = fs.expect("a modulus has a frame state");
                let ry = self.gpr(y, RCX);
                self.a.mov_rr32(RCX, ry);
                self.a.test_rr(false, RCX, RCX);
                self.exit_if(Cc::E, fs, Reason::Int);
                self.a.alu_ri(Alu::Cmp, false, RCX, -1);
                self.exit_if(Cc::E, fs, Reason::Int);
                let rx = self.gpr(x, RAX);
                self.a.mov_rr32(RAX, rx);
                self.a.mov_rr32(R11, RAX);
                self.a.cdq_idiv(RCX);
                // 0 from a negative dividend is −0
                let ok = self.a.label();
                self.a.test_rr(false, RDX, RDX);
                self.a.jcc(Cc::Ne, ok);
                self.a.test_rr(false, R11, R11);
                self.exit_if(Cc::L, fs, Reason::Int);
                self.a.bind(ok);
                self.put(v, RDX);
            }
            IntOp::Shl | IntOp::Sar | IntOp::Shr => {
                let kind = match op {
                    IntOp::Shl => 4,
                    IntOp::Sar => 7,
                    _ => 5,
                };
                let rx = self.gpr(x, RAX);
                self.a.mov_rr32(RAX, rx);
                match imm {
                    Some(i) => self.a.shift_ri(kind, false, RAX, (i & 31) as u8),
                    None => {
                        let ry = self.gpr(y, RCX);
                        self.a.mov_rr32(RCX, ry);
                        self.a.shift_cl(kind, false, RAX);
                    }
                }
                if op == IntOp::Shr
                    && let Some(fs) = fs
                {
                    self.a.test_rr(false, RAX, RAX);
                    self.exit_if(Cc::L, fs, Reason::Int);
                }
                self.put(v, RAX);
            }
        }
    }

    /// The parallel copy of `to`'s phi inputs from block `from`
    fn phi_moves(&mut self, from: B, to: B) {
        let blk = &self.g.blocks[to as usize];
        let Some(k) = blk.preds.iter().position(|&p| p == from) else { return };
        let mut moves: Vec<(Loc, V, Loc)> = Vec::new();
        for &p in &blk.insts {
            if let Op::Phi(ins) = self.g.op(p) {
                let src = ins[k];
                let (s, d) = (self.loc(src), self.loc(p));
                if s != d || s == Loc::Const {
                    moves.push((s, src, d));
                }
            }
        }
        // Sequentialize: a move whose destination no pending move reads goes first; cycles go through memory
        let mut pending = moves;
        while !pending.is_empty() {
            let pos = pending.iter().position(|(_, _, d)| !pending.iter().any(|(s, _, _)| s == d && *s != Loc::Const));
            match pos {
                Some(i) => {
                    let (_, src, d) = pending.remove(i);
                    self.move_to(src, d);
                }
                None => {
                    // A cycle: park the first source in Ctx::ARGS
                    let (s, src, _) = pending[0];
                    let r = self.gpr(src, RAX);
                    self.a.store(R15, Ctx::ARGS + 8 * 15, r);
                    for m in &mut pending {
                        if m.0 == s {
                            m.0 = Loc::Stack(i32::MIN);
                        }
                    }
                    // Moves from the parked value read it back
                    let parked: Vec<(V, Loc)> = pending.iter().filter(|m| m.0 == Loc::Stack(i32::MIN)).map(|m| (m.1, m.2)).collect();
                    pending.retain(|m| m.0 != Loc::Stack(i32::MIN));
                    let rest = core::mem::take(&mut pending);
                    pending = rest;
                    // Do the others first, then the parked ones
                    while !pending.is_empty() {
                        let (_, src, d) = pending.remove(0);
                        self.move_to(src, d);
                    }
                    for (_, d) in parked {
                        self.a.load(RAX, R15, Ctx::ARGS + 8 * 15);
                        match d {
                            Loc::Gpr(r) => self.a.mov_rr(r, RAX),
                            Loc::Stack(o) => self.a.store(RBP, o, RAX),
                            Loc::Xmm(x) => self.a.movq_xr(x, RAX),
                            _ => {}
                        }
                    }
                }
            }
        }
    }

    fn move_to(&mut self, src: V, d: Loc) {
        match d {
            Loc::Gpr(r) => self.into_gpr(src, r),
            Loc::Stack(o) => {
                if let Loc::Xmm(x) = self.loc(src) {
                    self.a.movsd_store(RBP, o, x);
                } else {
                    let r = self.gpr(src, RAX);
                    self.a.store(RBP, o, r);
                }
            }
            Loc::Xmm(x) => {
                let s = self.xmm(src, XS);
                self.a.movsd_xx(x, s);
            }
            _ => {}
        }
    }
}
