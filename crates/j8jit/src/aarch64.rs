use std::collections::BTreeMap;
use std::format;
use std::string::String;
use std::vec::Vec;

use mlirpass::mlirformat::llvm::{FCmpPredicate, ICmpPredicate};
use mlirpass::mlirformat::llvm_types::LlvmType;
use mlirpass::mlirformat::{Attribute, Error, FloatKind, Type};
use mlirpass::{Attrs, Body, BlockId, Hole, HoleKind, OpId, Stencil, StencilEmit, ValueId};

use crate::live::{Interval, Predicate, Template};

pub struct AArch64Emit;

impl StencilEmit for AArch64Emit {
    fn stencils(&self, body: &Body<'_>, root: OpId) -> Result<Vec<Stencil>, Error> {
        let mut out = Vec::new();
        for (name, f) in Template::stencils(body, root) {
            let mut a = A64::new(body, f).map_err(|e| Error(format!("aarch64: stencil @{name}: {}", e.0)))?;
            a.function(f).map_err(|e| Error(format!("aarch64: stencil @{name}: {}", e.0)))?;
            out.push(Stencil { name, code: a.code, holes: a.holes });
        }
        Ok(out)
    }
}

type Reg = u32;

struct X;

impl X {
    const ZR: Reg = 31;
    const SP: Reg = 31;
    const FP: Reg = 29;
    const LR: Reg = 30;
    const CTX: Reg = 23;
    const T0: Reg = 16;
    const T1: Reg = 17;
    const SCRATCH: [Reg; 10] = [6, 7, 8, 9, 10, 11, 12, 13, 14, 15];
    const SAVED: [Reg; 4] = [19, 20, 21, 22];
    const ARGS: [Reg; 8] = [0, 1, 2, 3, 4, 5, 6, 7];
    const FLOATS: [Reg; 8] = [16, 17, 18, 19, 20, 21, 22, 23];
    const F0: Reg = 30;
    const F1: Reg = 31;
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Loc {
    X(Reg),
    D(Reg),
    Slot(i32),
}

#[derive(Clone, Copy)]
enum Fix {
    Branch,
    Cond,
}

#[derive(Clone, Copy)]
#[repr(u32)]
enum Cond {
    Eq = 0,
    Ne = 1,
    Hs = 2,
    Lo = 3,
    Mi = 4,
    Pl = 5,
    Vs = 6,
    Vc = 7,
    Hi = 8,
    Ls = 9,
    Ge = 10,
    Lt = 11,
    Gt = 12,
    Le = 13,
}

#[derive(Clone, Copy)]
enum Width {
    B,
    H,
    W,
    X,
    D,
}

impl Width {
    fn load_reg(self) -> u32 {
        match self {
            Width::B => 0x3860_6800,
            Width::H => 0x7860_6800,
            Width::W => 0xB860_6800,
            Width::X => 0xF860_6800,
            Width::D => 0xFC60_6800,
        }
    }

    fn store_reg(self) -> u32 {
        self.load_reg() & !0x0040_0000
    }

    fn load_zero(self) -> u32 {
        match self {
            Width::B => 0x3940_0000,
            Width::H => 0x7940_0000,
            Width::W => 0xB940_0000,
            Width::X => 0xF940_0000,
            Width::D => 0xFD40_0000,
        }
    }

    fn store_zero(self) -> u32 {
        self.load_zero() & !0x0040_0000
    }
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Ty {
    Int(u32),
    F64,
}

struct A64<'b, 'a> {
    body: &'b Body<'a>,
    code: Vec<u8>,
    locs: BTreeMap<ValueId, Loc>,
    slots: BTreeMap<ValueId, String>,
    holes: Vec<Hole>,
    blocks: BTreeMap<BlockId, usize>,
    fixups: Vec<(usize, BlockId, Fix)>,
    ends: Vec<usize>,
}

impl<'b, 'a> A64<'b, 'a> {
    fn new(body: &'b Body<'a>, f: OpId) -> Result<Self, Error> {
        let slots = Template::slots(body, f).ok_or_else(|| Error("not a stencil".into()))?;
        let region = body[f].regions[0];
        let blocks: Vec<BlockId> = body[region].blocks.iter().copied().collect();
        let args = &body[blocks[0]].args;
        if args.len() != 2 {
            return Err(Error("a stencil takes the context and the frame".into()));
        }
        let mut locs = BTreeMap::new();
        locs.insert(args[0], Loc::X(X::CTX));
        locs.insert(args[1], Loc::X(X::FP));
        let mut intervals = Interval::of(body, &blocks)?;
        intervals.retain(|iv| !slots.contains_key(&iv.value) && !locs.contains_key(&iv.value));
        let mut active: Vec<(u32, Loc)> = Vec::new();
        let mut next = Template::SPILL_TOP;
        for iv in &intervals {
            active.retain(|&(hi, _)| hi >= iv.lo);
            let busy = |l: Loc| active.iter().any(|&(_, a)| a == l);
            let pick = if iv.float {
                if iv.across_call { None } else { X::FLOATS.iter().map(|&r| Loc::D(r)).find(|&l| !busy(l)) }
            } else {
                let scratch: &[Reg] = if iv.across_call { &[] } else { &X::SCRATCH };
                scratch.iter().chain(&X::SAVED).map(|&r| Loc::X(r)).find(|&l| !busy(l))
            };
            let l = match pick {
                Some(l) => {
                    active.push((iv.hi, l));
                    l
                }
                None => {
                    next -= 8;
                    Loc::Slot(next)
                }
            };
            locs.insert(iv.value, l);
        }
        if next < Template::SPILL_TOP - 8 * Template::SPILL_WORDS {
            return Err(Error("a stencil spills more than its words".into()));
        }
        Ok(A64 { body, code: Vec::new(), locs, slots, holes: Vec::new(), blocks: BTreeMap::new(), fixups: Vec::new(), ends: Vec::new() })
    }

    fn put(&mut self, w: u32) {
        self.code.extend_from_slice(&w.to_le_bytes());
    }

    fn here(&self) -> usize {
        self.code.len()
    }

    fn hole(&mut self, at: usize, kind: HoleKind, name: &str) {
        self.holes.push(Hole { at, kind, name: name.into() });
    }

    fn imm(&mut self, d: Reg, v: u64) {
        let chunks: [u64; 4] = core::array::from_fn(|i| v >> (16 * i) & 0xFFFF);
        let mut placed = false;
        for (i, &c) in chunks.iter().enumerate() {
            if c == 0 {
                continue;
            }
            let hw = u32::try_from(i).unwrap_or(0) << 21;
            let c = u32::try_from(c).unwrap_or(0) << 5;
            self.put(if placed { 0xF280_0000 } else { 0xD280_0000 } | hw | c | d);
            placed = true;
        }
        if !placed {
            self.put(0xD280_0000 | d);
        }
    }

    fn mov(&mut self, d: Reg, s: Reg) {
        if d != s {
            self.put(0xAA00_03E0 | s << 16 | d);
        }
    }

    fn fmov(&mut self, d: Reg, s: Reg) {
        if d != s {
            self.put(0x1E60_4000 | s << 5 | d);
        }
    }

    fn ldur(&mut self, float: bool, t: Reg, disp: i32) {
        let op = if float { 0xFC40_0000 } else { 0xF840_0000 };
        self.put(op | (disp.cast_unsigned() & 0x1FF) << 12 | X::FP << 5 | t);
    }

    fn stur(&mut self, float: bool, t: Reg, disp: i32) {
        let op = if float { 0xFC00_0000 } else { 0xF800_0000 };
        self.put(op | (disp.cast_unsigned() & 0x1FF) << 12 | X::FP << 5 | t);
    }

    fn mask(&mut self, r: Reg, w: u32) {
        if w < 64 {
            self.put(0xD340_0000 | (w - 1) << 10 | r << 5 | r);
        }
    }

    fn sext(&mut self, d: Reg, s: Reg, w: u32) {
        if w < 64 {
            self.put(0x9340_0000 | (w - 1) << 10 | s << 5 | d);
        } else {
            self.mov(d, s);
        }
    }

    fn literal64(&mut self, d: Reg, name: &str) {
        self.put(0x5800_0040 | d);
        self.put(0x1400_0003);
        let at = self.here();
        self.code.extend_from_slice(&[0; 8]);
        self.hole(at, HoleKind::Abs64, name);
    }

    fn frame_index(&mut self, name: &str) {
        self.put(0x9800_0040 | X::T0);
        self.put(0x1400_0002);
        let at = self.here();
        self.code.extend_from_slice(&[0; 4]);
        self.hole(at, HoleKind::Disp32, name);
    }

    fn ty(&self, v: ValueId) -> Result<Ty, Error> {
        match &self.body[v].ty {
            Type::Integer { width, .. } if *width <= 64 => Ok(Ty::Int(*width)),
            Type::Float(FloatKind::F64) => Ok(Ty::F64),
            Type::Index => Ok(Ty::Int(64)),
            t if matches!(LlvmType::try_from(t), Ok(LlvmType::Ptr(_))) => Ok(Ty::Int(64)),
            t => Err(Error(format!("values of type {t} are not supported"))),
        }
    }

    fn width(&self, v: ValueId) -> Result<u32, Error> {
        match self.ty(v)? {
            Ty::Int(w) => Ok(w),
            Ty::F64 => Ok(64),
        }
    }

    fn loc(&self, v: ValueId) -> Result<Loc, Error> {
        self.locs.get(&v).copied().ok_or_else(|| Error("a value without a location".into()))
    }

    fn get(&mut self, v: ValueId, scratch: Reg) -> Result<Reg, Error> {
        Ok(match self.loc(v)? {
            Loc::X(r) => r,
            Loc::Slot(d) => {
                self.ldur(false, scratch, d);
                scratch
            }
            Loc::D(r) => {
                self.put(0x9E66_0000 | r << 5 | scratch);
                scratch
            }
        })
    }

    fn getd(&mut self, v: ValueId, scratch: Reg) -> Result<Reg, Error> {
        Ok(match self.loc(v)? {
            Loc::D(r) => r,
            Loc::Slot(d) => {
                self.ldur(true, scratch, d);
                scratch
            }
            Loc::X(r) => {
                self.put(0x9E67_0000 | r << 5 | scratch);
                scratch
            }
        })
    }

    fn dst(&self, v: ValueId) -> Result<Reg, Error> {
        Ok(match self.loc(v)? {
            Loc::X(r) => r,
            _ => X::T0,
        })
    }

    fn dstd(&self, v: ValueId) -> Result<Reg, Error> {
        Ok(match self.loc(v)? {
            Loc::D(r) => r,
            _ => X::F0,
        })
    }

    fn set(&mut self, v: ValueId, r: Reg) -> Result<(), Error> {
        match self.loc(v)? {
            Loc::X(d) => self.mov(d, r),
            Loc::Slot(d) => self.stur(false, r, d),
            Loc::D(d) => self.put(0x9E67_0000 | r << 5 | d),
        }
        Ok(())
    }

    fn setd(&mut self, v: ValueId, r: Reg) -> Result<(), Error> {
        match self.loc(v)? {
            Loc::D(d) => self.fmov(d, r),
            Loc::Slot(d) => self.stur(true, r, d),
            Loc::X(d) => self.put(0x9E66_0000 | r << 5 | d),
        }
        Ok(())
    }

    fn operand(&self, op: OpId, i: usize) -> Result<ValueId, Error> {
        self.body[op].operands.get(i).copied().ok_or_else(|| Error(format!("{} needs operand {i}", self.body[op].name)))
    }

    fn result(&self, op: OpId) -> Result<ValueId, Error> {
        self.body[op].results.first().copied().ok_or_else(|| Error(format!("{} has no result", self.body[op].name)))
    }

    fn function(&mut self, f: OpId) -> Result<(), Error> {
        let body = self.body;
        let region = body[f].regions[0];
        let blocks: Vec<BlockId> = body[region].blocks.iter().copied().collect();
        for &b in &blocks {
            self.blocks.insert(b, self.here());
            for op in body.block_ops(b) {
                self.op(op)?;
            }
        }
        for (at, b, fix) in core::mem::take(&mut self.fixups) {
            let to = self.blocks[&b];
            self.patch(at, to, fix);
        }
        let mut ends = core::mem::take(&mut self.ends);
        if let Some(last) = ends.iter().position(|&at| at + 4 == self.here()) {
            let at = ends.swap_remove(last);
            self.code.truncate(at);
        }
        let end = self.here();
        for at in ends {
            self.patch(at, end, Fix::Branch);
        }
        Ok(())
    }

    fn patch(&mut self, at: usize, to: usize, fix: Fix) {
        let words = (i64::try_from(to).unwrap_or(0) - i64::try_from(at).unwrap_or(0)) / 4;
        let mut insn = u32::from_le_bytes([self.code[at], self.code[at + 1], self.code[at + 2], self.code[at + 3]]);
        insn |= match fix {
            Fix::Branch => (words as u32) & 0x03FF_FFFF,
            Fix::Cond => ((words as u32) & 0x7FFFF) << 5,
        };
        self.code[at..at + 4].copy_from_slice(&insn.to_le_bytes());
    }

    fn op(&mut self, op: OpId) -> Result<(), Error> {
        let body = self.body;
        if let Some(&r) = body[op].results.first()
            && self.slots.contains_key(&r)
        {
            return Ok(());
        }
        let name: &str = &body[op].name;
        match name {
            "llvm.mlir.constant" => self.constant(op),
            "llvm.mlir.zero" | "llvm.mlir.undef" | "llvm.mlir.poison" => {
                let r = self.result(op)?;
                self.set(r, X::ZR)
            }
            "llvm.add" | "llvm.sub" | "llvm.mul" | "llvm.and" | "llvm.or" | "llvm.xor" | "llvm.shl" | "llvm.lshr" | "llvm.ashr" | "llvm.srem" | "llvm.sdiv" => self.int(op, name),
            "llvm.icmp" => self.icmp(op),
            "llvm.fcmp" => self.fcmp(op),
            "llvm.fadd" | "llvm.fsub" | "llvm.fmul" | "llvm.fdiv" => self.float(op, name),
            "llvm.select" => self.select(op),
            "llvm.sext" => {
                let (a, r) = (self.operand(op, 0)?, self.result(op)?);
                let (wa, wr) = (self.width(a)?, self.width(r)?);
                let s = self.get(a, X::T0)?;
                let d = self.dst(r)?;
                self.sext(d, s, wa);
                self.mask(d, wr);
                self.set(r, d)
            }
            "llvm.zext" | "llvm.trunc" | "llvm.inttoptr" | "llvm.ptrtoint" => {
                let (a, r) = (self.operand(op, 0)?, self.result(op)?);
                let wr = self.width(r)?;
                let s = self.get(a, X::T0)?;
                let d = self.dst(r)?;
                self.mov(d, s);
                if name == "llvm.trunc" {
                    self.mask(d, wr);
                }
                self.set(r, d)
            }
            "llvm.bitcast" => {
                let (a, r) = (self.operand(op, 0)?, self.result(op)?);
                match self.ty(r)? {
                    Ty::F64 => {
                        let s = self.get(a, X::T0)?;
                        let d = self.dstd(r)?;
                        self.put(0x9E67_0000 | s << 5 | d);
                        self.setd(r, d)
                    }
                    Ty::Int(_) => {
                        let s = self.getd(a, X::F0)?;
                        let d = self.dst(r)?;
                        self.put(0x9E66_0000 | s << 5 | d);
                        self.set(r, d)
                    }
                }
            }
            "llvm.sitofp" => {
                let (a, r) = (self.operand(op, 0)?, self.result(op)?);
                let w = self.width(a)?;
                let s = self.get(a, X::T0)?;
                self.sext(X::T0, s, w);
                let d = self.dstd(r)?;
                self.put(0x9E62_0000 | X::T0 << 5 | d);
                self.setd(r, d)
            }
            "llvm.fptosi" => {
                let (a, r) = (self.operand(op, 0)?, self.result(op)?);
                let w = self.width(r)?;
                let s = self.getd(a, X::F0)?;
                let d = self.dst(r)?;
                self.put(0x9E78_0000 | s << 5 | d);
                self.mask(d, w);
                self.set(r, d)
            }
            "llvm.load" => self.load(op),
            "llvm.store" => self.store(op),
            "llvm.call" => self.call(op),
            "llvm.br" => {
                if !body[op].operands.is_empty() {
                    return Err(Error("a stencil passes no block arguments".into()));
                }
                let to = *body[op].successors.first().ok_or_else(|| Error("llvm.br without a successor".into()))?;
                self.branch(to, Fix::Branch, 0x1400_0000);
                Ok(())
            }
            "llvm.cond_br" => {
                if body[op].operands.len() != 1 {
                    return Err(Error("a stencil passes no block arguments".into()));
                }
                let succ: Vec<BlockId> = body[op].successors.iter().copied().collect();
                let [t, f] = succ[..] else { return Err(Error("llvm.cond_br needs two successors".into())) };
                let c = self.get(self.operand(op, 0)?, X::T0)?;
                self.branch(t, Fix::Cond, 0xB500_0000 | c);
                self.branch(f, Fix::Branch, 0x1400_0000);
                Ok(())
            }
            "llvm.return" => {
                if !body[op].operands.is_empty() {
                    return Err(Error("a stencil returns nothing".into()));
                }
                self.ends.push(self.here());
                self.put(0x1400_0000);
                Ok(())
            }
            "llvm.unreachable" => Ok(()),
            "llvm.intr.stacksave" => {
                let r = self.result(op)?;
                let d = self.dst(r)?;
                self.put(0x9100_0000 | X::SP << 5 | d);
                self.set(r, d)
            }
            other => Err(Error(format!("{other} is not supported"))),
        }
    }

    fn branch(&mut self, to: BlockId, fix: Fix, insn: u32) {
        self.fixups.push((self.here(), to, fix));
        self.put(insn);
    }

    fn constant(&mut self, op: OpId) -> Result<(), Error> {
        let r = self.result(op)?;
        let value = self.body[op].get("value").ok_or_else(|| Error("llvm.mlir.constant without a value".into()))?;
        let ty = self.ty(r)?;
        let bits: u64 = match (value, ty) {
            (Attribute::Bool(b), _) => u64::from(*b),
            (Attribute::Float { lit, .. }, Ty::F64) => match lit.strip_prefix("0x").or_else(|| lit.strip_prefix("0X")) {
                Some(h) => u64::from_str_radix(h, 16).map_err(|_| Error(format!("bad float {lit}")))?,
                None => lit.parse::<f64>().map_err(|_| Error(format!("bad float {lit}")))?.to_bits(),
            },
            (Attribute::Int { lit, .. }, _) => match lit.strip_prefix("0x") {
                Some(h) => u64::from_str_radix(h, 16).map_err(|_| Error(format!("bad integer {lit}")))?,
                None => Attrs::as_int(value)?.cast_unsigned(),
            },
            (a, _) => return Err(Error(format!("constant {a} is not supported"))),
        };
        match ty {
            Ty::Int(w) => {
                let bits = if w < 64 { bits & ((1u64 << w) - 1) } else { bits };
                let d = self.dst(r)?;
                self.imm(d, bits);
                self.set(r, d)
            }
            Ty::F64 => {
                self.imm(X::T0, bits);
                let d = self.dstd(r)?;
                self.put(0x9E67_0000 | X::T0 << 5 | d);
                self.setd(r, d)
            }
        }
    }

    fn int(&mut self, op: OpId, name: &str) -> Result<(), Error> {
        let r = self.result(op)?;
        let w = self.width(r)?;
        let a = self.get(self.operand(op, 0)?, X::T0)?;
        let b = self.get(self.operand(op, 1)?, X::T1)?;
        let d = self.dst(r)?;
        let rr = |base: u32| base | b << 16 | a << 5 | d;
        match name {
            "llvm.add" => self.put(rr(0x8B00_0000)),
            "llvm.sub" => self.put(rr(0xCB00_0000)),
            "llvm.mul" => self.put(rr(0x9B00_7C00)),
            "llvm.and" => self.put(rr(0x8A00_0000)),
            "llvm.or" => self.put(rr(0xAA00_0000)),
            "llvm.xor" => self.put(rr(0xCA00_0000)),
            "llvm.shl" => self.put(rr(0x9AC0_2000)),
            "llvm.lshr" => self.put(rr(0x9AC0_2400)),
            "llvm.ashr" => {
                self.sext(X::T0, a, w);
                self.put(0x9AC0_2800 | b << 16 | X::T0 << 5 | d);
            }
            _ => {
                self.sext(X::T0, a, w);
                self.sext(X::T1, b, w);
                self.put(0x9AC0_0C00 | X::T1 << 16 | X::T0 << 5 | X::LR);
                if name == "llvm.srem" {
                    self.put(0x9B00_8000 | X::T1 << 16 | X::T0 << 10 | X::LR << 5 | d);
                } else {
                    self.mov(d, X::LR);
                }
            }
        }
        self.mask(d, w);
        self.set(r, d)
    }

    fn icmp(&mut self, op: OpId) -> Result<(), Error> {
        let r = self.result(op)?;
        let pred = ICmpPredicate::of(self.predicate(op)?)?;
        let (x, y) = (self.operand(op, 0)?, self.operand(op, 1)?);
        let w = self.width(x)?;
        let mut a = self.get(x, X::T0)?;
        let mut b = self.get(y, X::T1)?;
        let signed = matches!(pred, ICmpPredicate::Slt | ICmpPredicate::Sle | ICmpPredicate::Sgt | ICmpPredicate::Sge);
        if signed && w < 64 {
            self.sext(X::T0, a, w);
            self.sext(X::T1, b, w);
            (a, b) = (X::T0, X::T1);
        }
        self.put(0xEB00_001F | b << 16 | a << 5);
        let c = match pred {
            ICmpPredicate::Eq => Cond::Eq,
            ICmpPredicate::Ne => Cond::Ne,
            ICmpPredicate::Slt => Cond::Lt,
            ICmpPredicate::Sle => Cond::Le,
            ICmpPredicate::Sgt => Cond::Gt,
            ICmpPredicate::Sge => Cond::Ge,
            ICmpPredicate::Ult => Cond::Lo,
            ICmpPredicate::Ule => Cond::Ls,
            ICmpPredicate::Ugt => Cond::Hi,
            ICmpPredicate::Uge => Cond::Hs,
        };
        let d = self.dst(r)?;
        self.cset(d, c);
        self.set(r, d)
    }

    fn cset(&mut self, d: Reg, c: Cond) {
        self.put(0x9A9F_07E0 | (c as u32 ^ 1) << 12 | d);
    }

    fn fcmp(&mut self, op: OpId) -> Result<(), Error> {
        let r = self.result(op)?;
        let pred = FCmpPredicate::of(self.predicate(op)?)?;
        let d = self.dst(r)?;
        let both = |p: FCmpPredicate| -> Option<&[Cond]> {
            Some(match p {
                FCmpPredicate::Oeq => &[Cond::Eq],
                FCmpPredicate::Ogt => &[Cond::Gt],
                FCmpPredicate::Oge => &[Cond::Ge],
                FCmpPredicate::Olt => &[Cond::Mi],
                FCmpPredicate::Ole => &[Cond::Ls],
                FCmpPredicate::One => &[Cond::Mi, Cond::Gt],
                FCmpPredicate::Ord => &[Cond::Vc],
                FCmpPredicate::Ueq => &[Cond::Eq, Cond::Vs],
                FCmpPredicate::Ugt => &[Cond::Hi],
                FCmpPredicate::Uge => &[Cond::Pl],
                FCmpPredicate::Ult => &[Cond::Lt],
                FCmpPredicate::Ule => &[Cond::Le],
                FCmpPredicate::Une => &[Cond::Ne],
                FCmpPredicate::Uno => &[Cond::Vs],
                FCmpPredicate::False | FCmpPredicate::True => return None,
            })
        };
        match both(pred) {
            None => self.imm(d, u64::from(pred == FCmpPredicate::True)),
            Some(conds) => {
                let a = self.getd(self.operand(op, 0)?, X::F0)?;
                let b = self.getd(self.operand(op, 1)?, X::F1)?;
                self.put(0x1E60_2000 | b << 16 | a << 5);
                self.cset(d, conds[0]);
                if let Some(&c) = conds.get(1) {
                    self.cset(X::T1, c);
                    self.put(0xAA00_0000 | X::T1 << 16 | d << 5 | d);
                }
            }
        }
        self.set(r, d)
    }

    fn predicate(&self, op: OpId) -> Result<i64, Error> {
        match self.body[op].get("predicate") {
            Some(a @ Attribute::Int { .. }) => Attrs::as_int(a),
            _ => Err(Error(format!("{} without a predicate", self.body[op].name))),
        }
    }

    fn float(&mut self, op: OpId, name: &str) -> Result<(), Error> {
        let r = self.result(op)?;
        let a = self.getd(self.operand(op, 0)?, X::F0)?;
        let b = self.getd(self.operand(op, 1)?, X::F1)?;
        let d = self.dstd(r)?;
        let code = match name {
            "llvm.fadd" => 0x1E60_2800,
            "llvm.fsub" => 0x1E60_3800,
            "llvm.fmul" => 0x1E60_0800,
            _ => 0x1E60_1800,
        };
        self.put(code | b << 16 | a << 5 | d);
        self.setd(r, d)
    }

    fn select(&mut self, op: OpId) -> Result<(), Error> {
        let r = self.result(op)?;
        if self.ty(r)? == Ty::F64 {
            return Err(Error("a float select".into()));
        }
        let c = self.get(self.operand(op, 0)?, X::T0)?;
        self.put(0xEB1F_001F | c << 5);
        let t = self.get(self.operand(op, 1)?, X::T0)?;
        let f = self.get(self.operand(op, 2)?, X::T1)?;
        let d = self.dst(r)?;
        self.put(0x9A80_0000 | f << 16 | (Cond::Ne as u32) << 12 | t << 5 | d);
        self.set(r, d)
    }

    fn access(&self, v: ValueId) -> Result<Width, Error> {
        Ok(match self.ty(v)? {
            Ty::F64 => Width::D,
            Ty::Int(w) if w <= 8 => Width::B,
            Ty::Int(w) if w <= 16 => Width::H,
            Ty::Int(w) if w <= 32 => Width::W,
            Ty::Int(_) => Width::X,
        })
    }

    fn load(&mut self, op: OpId) -> Result<(), Error> {
        let r = self.result(op)?;
        let addr = self.operand(op, 0)?;
        let width = self.access(r)?;
        let float = matches!(width, Width::D);
        let d = if float { self.dstd(r)? } else { self.dst(r)? };
        match self.slots.get(&addr).cloned() {
            Some(name) => {
                self.frame_index(&name);
                self.put(width.load_reg() | X::T0 << 16 | X::FP << 5 | d);
            }
            None => {
                let a = self.get(addr, X::T0)?;
                self.put(width.load_zero() | a << 5 | d);
            }
        }
        if float { self.setd(r, d) } else { self.set(r, d) }
    }

    fn store(&mut self, op: OpId) -> Result<(), Error> {
        let (v, addr) = (self.operand(op, 0)?, self.operand(op, 1)?);
        let width = self.access(v)?;
        let t = if matches!(width, Width::D) { self.getd(v, X::F0)? } else { self.get(v, X::T1)? };
        match self.slots.get(&addr).cloned() {
            Some(name) => {
                self.frame_index(&name);
                self.put(width.store_reg() | X::T0 << 16 | X::FP << 5 | t);
            }
            None => {
                let a = self.get(addr, X::T0)?;
                self.put(width.store_zero() | a << 5 | t);
            }
        }
        Ok(())
    }

    fn call(&mut self, op: OpId) -> Result<(), Error> {
        let callee = Template::callee(self.body, op).ok_or_else(|| Error("an indirect call is not supported".into()))?.to_owned();
        if callee.starts_with(Template::SLOT) {
            return Ok(());
        }
        if callee.starts_with(Template::JUMP) {
            let at = self.here();
            self.put(0x1400_0000);
            self.hole(at, HoleKind::Branch26, &callee);
            return Ok(());
        }
        if callee.starts_with(Template::HOLE) {
            let r = self.result(op)?;
            let d = self.dst(r)?;
            self.literal64(d, &callee);
            return self.set(r, d);
        }
        let args: Vec<ValueId> = self.body[op].operands.iter().copied().collect();
        if args.len() > X::ARGS.len() {
            return Err(Error("a call with more than eight arguments".into()));
        }
        for (i, &a) in args.iter().enumerate() {
            if self.ty(a)? == Ty::F64 {
                return Err(Error("a float argument".into()));
            }
            let s = self.get(a, X::T0)?;
            self.mov(X::ARGS[i], s);
        }
        self.literal64(X::T0, &callee);
        self.put(0xD63F_0000 | X::T0 << 5);
        if let Some(&r) = self.body[op].results.first() {
            self.set(r, X::ARGS[0])?;
        }
        Ok(())
    }
}
