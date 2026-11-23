use std::collections::{BTreeMap, HashMap};
use std::format;
use std::string::{String, ToString};
use std::vec::Vec;

use mlirpass::mlirformat::llvm::{FCmpPredicate, ICmpPredicate};
use mlirpass::mlirformat::llvm_types::LlvmType;
use mlirpass::mlirformat::{Attribute, Error, FloatKind, Type};

use mlirpass::{Attrs, Body, BlockId, Emit, Hole, HoleKind, OpId, Stencil, StencilEmit, ValueId};

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct X86_64Code {
    pub name: String,
    pub bytes: Vec<u8>,
    pub relocs: Vec<X86_64Reloc>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct X86_64Reloc {
    pub offset: usize,
    pub symbol: String,
}

pub struct X86_64Emit;

impl Emit for X86_64Emit {
    type Output = Vec<X86_64Code>;

    fn emit(&self, body: &Body<'_>, op: OpId) -> Result<Vec<X86_64Code>, Error> {
        let mut out = Vec::new();
        Self::collect(body, op, &mut out)?;
        Ok(out)
    }
}

impl X86_64Emit {
    fn collect(body: &Body<'_>, op: OpId, out: &mut Vec<X86_64Code>) -> Result<(), Error> {
        if body[op].name == "llvm.func" {
            if let Some(&region) = body[op].regions.first()
                && !body[region].blocks.is_empty()
            {
                out.push(X86_64Code::of(body, op)?);
            }
            return Ok(());
        }
        for &region in body[op].regions.iter() {
            for &block in body[region].blocks.iter() {
                for child in body.block_ops(block) {
                    Self::collect(body, child, out)?;
                }
            }
        }
        Ok(())
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[repr(u8)]
enum Gpr {
    Rax = 0,
    Rcx = 1,
    Rdx = 2,
    Rbx = 3,
    Rsp = 4,
    Rbp = 5,
    Rsi = 6,
    Rdi = 7,
    R8 = 8,
    R9 = 9,
    R10 = 10,
    R11 = 11,
    R12 = 12,
    R13 = 13,
    R14 = 14,
    R15 = 15,
}

impl Gpr {
    const ARGS: [Gpr; 6] = [Gpr::Rdi, Gpr::Rsi, Gpr::Rdx, Gpr::Rcx, Gpr::R8, Gpr::R9];

    fn low(self) -> u8 {
        self as u8 & 7
    }

    fn high(self) -> bool {
        self as u8 >= 8
    }
}

const XMM_ARGS: u8 = 8;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Kind {
    Int(u32),
    F64,
}

impl Kind {
    fn of(t: &Type<'_>) -> Result<Kind, Error> {
        match t {
            Type::Integer { width, .. } if *width <= 64 => Ok(Kind::Int(*width)),
            Type::Float(FloatKind::F64) => Ok(Kind::F64),
            Type::Index => Ok(Kind::Int(64)),
            t if matches!(LlvmType::try_from(t), Ok(LlvmType::Ptr(_))) => Ok(Kind::Int(64)),
            t => Err(Error(format!("x86-64: values of type {t} are not supported"))),
        }
    }

    fn size(t: &Type<'_>) -> Result<u32, Error> {
        match Kind::of(t)? {
            Kind::Int(w) => Ok(w.div_ceil(8).next_power_of_two()),
            Kind::F64 => Ok(8),
        }
    }
}

#[derive(Default)]
struct Asm {
    bytes: Vec<u8>,
}

impl Asm {
    fn byte(&mut self, b: u8) {
        self.bytes.push(b);
    }

    fn bytes(&mut self, b: &[u8]) {
        self.bytes.extend_from_slice(b);
    }

    fn here(&self) -> usize {
        self.bytes.len()
    }

    fn rex(&mut self, w: bool, r: bool, b: bool) {
        let rex = 0x40 | (u8::from(w) << 3) | (u8::from(r) << 2) | u8::from(b);
        if rex != 0x40 {
            self.byte(rex);
        }
    }

    fn mem(&mut self, reg: u8, base: Gpr, disp: i32) {
        self.byte(0x80 | ((reg & 7) << 3) | base.low());
        self.bytes(&disp.to_le_bytes());
    }

    fn rm(&mut self, prefix: Option<u8>, w: bool, opcode: &[u8], reg: u8, base: Gpr, disp: i32) {
        if let Some(p) = prefix {
            self.byte(p);
        }
        self.rex(w, reg >= 8, base.high());
        self.bytes(opcode);
        self.mem(reg, base, disp);
    }

    fn rr(&mut self, prefix: Option<u8>, w: bool, opcode: &[u8], reg: u8, rm: u8) {
        if let Some(p) = prefix {
            self.byte(p);
        }
        self.rex(w, reg >= 8, rm >= 8);
        self.bytes(opcode);
        self.byte(0xc0 | ((reg & 7) << 3) | (rm & 7));
    }

    fn load64(&mut self, dst: Gpr, base: Gpr, disp: i32) {
        self.rm(None, true, &[0x8b], dst as u8, base, disp);
    }

    fn store64(&mut self, base: Gpr, disp: i32, src: Gpr) {
        self.rm(None, true, &[0x89], src as u8, base, disp);
    }

    fn load_int(&mut self, dst: Gpr, base: Gpr, disp: i32, bits: u32, signed: bool) {
        match (bits, signed) {
            (64, _) => self.load64(dst, base, disp),
            (32, false) => self.rm(None, false, &[0x8b], dst as u8, base, disp),
            (32, true) => self.rm(None, true, &[0x63], dst as u8, base, disp),
            (16, false) => self.rm(None, true, &[0x0f, 0xb7], dst as u8, base, disp),
            (16, true) => self.rm(None, true, &[0x0f, 0xbf], dst as u8, base, disp),
            (_, false) => self.rm(None, true, &[0x0f, 0xb6], dst as u8, base, disp),
            (_, true) => self.rm(None, true, &[0x0f, 0xbe], dst as u8, base, disp),
        }
    }

    fn store_int(&mut self, base: Gpr, disp: i32, src: Gpr, bytes: u32) {
        match bytes {
            8 => self.store64(base, disp, src),
            4 => self.rm(None, false, &[0x89], src as u8, base, disp),
            2 => self.rm(Some(0x66), false, &[0x89], src as u8, base, disp),
            _ => {
                self.rm(None, false, &[0x88], src as u8, base, disp);
            }
        }
    }

    fn mov_imm64(&mut self, dst: Gpr, imm: u64) -> usize {
        self.rex(true, false, dst.high());
        self.byte(0xb8 + dst.low());
        let at = self.here();
        self.bytes(&imm.to_le_bytes());
        at
    }

    fn mov_rr(&mut self, dst: Gpr, src: Gpr) {
        self.rr(None, true, &[0x89], src as u8, dst as u8);
    }

    fn movsd_load(&mut self, xmm: u8, base: Gpr, disp: i32) {
        self.rm(Some(0xf2), false, &[0x0f, 0x10], xmm, base, disp);
    }

    fn movsd_store(&mut self, base: Gpr, disp: i32, xmm: u8) {
        self.rm(Some(0xf2), false, &[0x0f, 0x11], xmm, base, disp);
    }

    fn setcc(&mut self, cc: u8) {
        self.bytes(&[0x0f, 0x90 | cc, 0xc0]);
        self.bytes(&[0x0f, 0xb6, 0xc0]);
    }

    fn jump(&mut self, cc: Option<u8>) -> usize {
        match cc {
            Some(cc) => self.bytes(&[0x0f, 0x80 | cc]),
            None => self.byte(0xe9),
        }
        let at = self.here();
        self.bytes(&[0; 4]);
        at
    }

    fn patch(&mut self, at: usize, target: usize) {
        let rel = i32::try_from(target as i64 - (at as i64 + 4)).unwrap_or(i32::MAX);
        self.bytes[at..at + 4].copy_from_slice(&rel.to_le_bytes());
    }
}

struct Cc;

impl Cc {
    const B: u8 = 0x2;
    const AE: u8 = 0x3;
    const E: u8 = 0x4;
    const NE: u8 = 0x5;
    const BE: u8 = 0x6;
    const A: u8 = 0x7;
    const P: u8 = 0xa;
    const NP: u8 = 0xb;
    const L: u8 = 0xc;
    const GE: u8 = 0xd;
    const LE: u8 = 0xe;
    const G: u8 = 0xf;
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Loc {
    Slot(i32),
    Gpr(Gpr),
    Xmm(u8),
}

struct Pool;

impl Pool {
    const SAVED: [Gpr; 5] = [Gpr::Rbx, Gpr::R12, Gpr::R13, Gpr::R14, Gpr::R15];
    const SCRATCH: [Gpr; 2] = [Gpr::R10, Gpr::R11];
    const XMM: [u8; 8] = [8, 9, 10, 11, 12, 13, 14, 15];
}

#[derive(Clone, Copy, Debug)]
struct Interval {
    value: ValueId,
    lo: u32,
    hi: u32,
    float: bool,
    across_call: bool,
}

struct Lower<'b, 'a> {
    body: &'b Body<'a>,
    asm: Asm,
    slots: BTreeMap<ValueId, Loc>,
    saved: Vec<(Gpr, i32)>,
    scratch: i32,
    frame: i32,
    blocks: BTreeMap<BlockId, usize>,
    fixups: Vec<(usize, BlockId)>,
    relocs: Vec<X86_64Reloc>,
    stencil: Option<Stenciled>,
}

#[derive(Default)]
pub struct Stenciled {
    holes: Vec<Hole>,
    slots: BTreeMap<ValueId, String>,
    ends: Vec<usize>,
}

impl Stenciled {
    pub const SPILL_TOP: i32 = crate::asm::SAVED_END;
    pub const SPILL_WORDS: i32 = 16;
    const ATTR: &str = "stencil";
    const HOLE: &str = "hole.";
    const SLOT: &str = "slot.";
    const JUMP: &str = "jump.";

    fn callee<'s>(body: &'s Body<'_>, op: OpId) -> Option<&'s str> {
        (body[op].name == "llvm.call").then(|| body[op].get("callee").and_then(|a| Attrs::as_symbol(a).ok())).flatten()
    }

    fn of(body: &Body<'_>, f: OpId) -> Option<Self> {
        if !body[f].has_flag(Self::ATTR) {
            return None;
        }
        let region = body[f].regions[0];
        let frame = body[region].blocks.first().and_then(|b| body[*b].args.get(1).copied());
        let mut me = Stenciled::default();
        for &b in body[region].blocks.iter() {
            for op in body.block_ops(b) {
                let virt = match Self::callee(body, op) {
                    Some(c) if c.starts_with(Self::SLOT) => Some(String::from(c)),
                    _ => match (&*body[op].name, body[op].operands.first()) {
                        ("llvm.add", Some(&a)) if Some(a) == frame => body[op].operands.get(1).and_then(|o| me.slots.get(o)).cloned(),
                        ("llvm.inttoptr", Some(a)) => me.slots.get(a).cloned(),
                        _ => None,
                    },
                };
                if let (Some(name), Some(&r)) = (virt, body[op].results.first()) {
                    me.slots.insert(r, name);
                }
            }
        }
        Some(me)
    }
}

impl X86_64Code {
    pub fn of(body: &Body<'_>, f: OpId) -> Result<X86_64Code, Error> {
        let name = body[f].get_str("sym_name").ok_or_else(|| Error("x86-64: a function without a name".into()))?.to_string();
        let mut lower = Lower::new(body, f).map_err(|e| Error(format!("x86-64: @{name}: {}", e.0)))?;
        lower.function(f).map_err(|e| Error(format!("x86-64: @{name}: {}", e.0)))?;
        Ok(X86_64Code { name, bytes: lower.asm.bytes, relocs: lower.relocs })
    }
}

impl<'b, 'a> Lower<'b, 'a> {
    fn new(body: &'b Body<'a>, f: OpId) -> Result<Self, Error> {
        let stencil = Stenciled::of(body, f);
        let region = body[f].regions[0];
        let blocks: Vec<BlockId> = body[region].blocks.iter().copied().collect();
        let mut widest_copy = 0usize;
        for &block in &blocks {
            for op in body.block_ops(block) {
                if !body[op].regions.is_empty() {
                    return Err(Error(format!("{} has regions", body[op].name)));
                }
                widest_copy = widest_copy.max(body[op].operands.len());
            }
        }
        let mut intervals = Self::intervals(body, &blocks)?;
        let mut fixed = BTreeMap::new();
        if let Some(st) = &stencil {
            let args = &body[blocks[0]].args;
            if args.len() != 2 {
                return Err(Error("a stencil takes the context and the frame".into()));
            }
            fixed.insert(args[0], Loc::Gpr(Gpr::R15));
            fixed.insert(args[1], Loc::Gpr(Gpr::Rbp));
            intervals.retain(|iv| !st.slots.contains_key(&iv.value) && !fixed.contains_key(&iv.value));
        }
        let (mut slots, used) = Self::allocate(&intervals, stencil.is_some());
        slots.extend(fixed);
        let mut next = if stencil.is_some() { Stenciled::SPILL_TOP } else { 0 };
        let mut saved = Vec::new();
        for r in used {
            next -= 8;
            saved.push((r, next));
        }
        for iv in &intervals {
            if let std::collections::btree_map::Entry::Vacant(e) = slots.entry(iv.value) {
                next -= 8;
                e.insert(Loc::Slot(next));
            }
        }
        if stencil.is_some() && next < Stenciled::SPILL_TOP - 8 * Stenciled::SPILL_WORDS {
            return Err(Error("a stencil spills more than its words".into()));
        }
        let scratch = next - 8 * i32::try_from(widest_copy).unwrap_or(0);
        let frame = (-scratch + 15) & !15;
        Ok(Lower { body, asm: Asm::default(), slots, saved, scratch, frame, blocks: BTreeMap::new(), fixups: Vec::new(), relocs: Vec::new(), stencil })
    }

    fn intervals(body: &Body<'_>, blocks: &[BlockId]) -> Result<Vec<Interval>, Error> {
        let mut index: HashMap<ValueId, u32> = HashMap::new();
        let mut values: Vec<ValueId> = Vec::new();
        let mut def: Vec<u32> = Vec::new();
        let mut used: Vec<u32> = Vec::new();
        let block_at: HashMap<BlockId, usize> = blocks.iter().enumerate().map(|(i, &b)| (b, i)).collect();
        let (mut start, mut end) = (std::vec![0u32; blocks.len()], std::vec![0u32; blocks.len()]);
        let mut calls = Vec::new();
        let mut uses: Vec<Vec<u32>> = std::vec![Vec::new(); blocks.len()];
        let mut defs: Vec<Vec<u32>> = std::vec![Vec::new(); blocks.len()];
        let mut succs: Vec<Vec<usize>> = std::vec![Vec::new(); blocks.len()];
        let mut pos = 0u32;
        let define = |v: ValueId, at: u32, index: &mut HashMap<ValueId, u32>, values: &mut Vec<ValueId>, def: &mut Vec<u32>, used: &mut Vec<u32>| -> u32 {
            let k = u32::try_from(values.len()).unwrap_or(u32::MAX);
            index.insert(v, k);
            values.push(v);
            def.push(at);
            used.push(at);
            k
        };
        for (bi, &b) in blocks.iter().enumerate() {
            start[bi] = pos;
            for &a in body[b].args.iter() {
                let k = define(a, pos, &mut index, &mut values, &mut def, &mut used);
                defs[bi].push(k);
            }
            for op in body.block_ops(b) {
                pos += 1;
                for &r in body[op].results.iter() {
                    let k = define(r, pos, &mut index, &mut values, &mut def, &mut used);
                    defs[bi].push(k);
                }
                if body[op].name == "llvm.call" {
                    calls.push(pos);
                }
                succs[bi] = body[op].successors.iter().filter_map(|s| block_at.get(s).copied()).collect();
            }
            end[bi] = pos;
            pos += 1;
        }
        let mut seen = std::vec![u32::MAX; values.len()];
        for (bi, &b) in blocks.iter().enumerate() {
            let mut at = start[bi];
            for &k in &defs[bi] {
                seen[k as usize] = u32::try_from(bi).unwrap_or(u32::MAX);
            }
            for op in body.block_ops(b) {
                at += 1;
                for &v in body[op].operands.iter() {
                    let k = *index.get(&v).ok_or_else(|| Error("a value defined outside the function".into()))?;
                    used[k as usize] = used[k as usize].max(at);
                    let local = seen[k as usize] == u32::try_from(bi).unwrap_or(u32::MAX) && def[k as usize] < at;
                    if !local {
                        uses[bi].push(k);
                    }
                }
            }
        }
        let n = values.len();
        let words = n.div_ceil(64);
        let bits = |list: &[u32]| {
            let mut w = std::vec![0u64; words];
            for &k in list {
                w[k as usize / 64] |= 1 << (k % 64);
            }
            w
        };
        let gen_: Vec<Vec<u64>> = uses.iter().map(|u| bits(u)).collect();
        let kill: Vec<Vec<u64>> = defs.iter().map(|d| bits(d)).collect();
        let mut live_in = std::vec![std::vec![0u64; words]; blocks.len()];
        let mut live_out = std::vec![std::vec![0u64; words]; blocks.len()];
        loop {
            let mut changed = false;
            for i in (0..blocks.len()).rev() {
                let mut out = std::vec![0u64; words];
                for &s in &succs[i] {
                    for (o, x) in out.iter_mut().zip(&live_in[s]) {
                        *o |= *x;
                    }
                }
                let inn: Vec<u64> = gen_[i].iter().zip(&out).zip(&kill[i]).map(|((g, o), k)| g | (o & !k)).collect();
                if out != live_out[i] || inn != live_in[i] {
                    changed = true;
                    live_out[i] = out;
                    live_in[i] = inn;
                }
            }
            if !changed {
                break;
            }
        }
        let (mut lo, mut hi): (Vec<u32>, Vec<u32>) = (def.clone(), def.iter().zip(&used).map(|(d, u)| (*d).max(*u)).collect());
        for i in 0..blocks.len() {
            for (wi, (&li, &lo_)) in live_in[i].iter().zip(&live_out[i]).enumerate() {
                let mut w = li;
                while w != 0 {
                    let k = wi * 64 + w.trailing_zeros() as usize;
                    w &= w - 1;
                    lo[k] = lo[k].min(start[i]);
                    hi[k] = hi[k].max(start[i]);
                }
                let mut w = lo_;
                while w != 0 {
                    let k = wi * 64 + w.trailing_zeros() as usize;
                    w &= w - 1;
                    hi[k] = hi[k].max(end[i]);
                }
            }
        }
        let mut out = Vec::with_capacity(n);
        for (k, &value) in values.iter().enumerate() {
            let kind = Kind::of(&body[value].ty)?;
            let float = kind == Kind::F64;
            let first = calls.partition_point(|&c| c <= lo[k]);
            let across_call = calls.get(first).is_some_and(|&c| c < hi[k]);
            out.push(Interval { value, lo: lo[k], hi: hi[k], float, across_call });
        }
        out.sort_by_key(|iv| (iv.lo, iv.hi));
        Ok(out)
    }

    fn allocate(intervals: &[Interval], stencil: bool) -> (BTreeMap<ValueId, Loc>, Vec<Gpr>) {
        let mut locs = BTreeMap::new();
        let mut active: Vec<(u32, Loc)> = Vec::new();
        let mut used: Vec<Gpr> = Vec::new();
        for iv in intervals {
            active.retain(|&(hi, _)| hi >= iv.lo);
            let busy = |l: Loc| active.iter().any(|&(_, a)| a == l);
            let pick = if iv.float {
                if iv.across_call { None } else { Pool::XMM.iter().map(|&x| Loc::Xmm(x)).find(|&l| !busy(l)) }
            } else {
                let scratch = (!iv.across_call).then_some(Pool::SCRATCH.iter()).into_iter().flatten();
                scratch.chain(Pool::SAVED.iter()).filter(|&&r| !stencil || r != Gpr::R15).map(|&r| Loc::Gpr(r)).find(|&l| !busy(l))
            };
            if let Some(l) = pick {
                if let Loc::Gpr(r) = l
                    && !stencil
                    && Pool::SAVED.contains(&r)
                    && !used.contains(&r)
                {
                    used.push(r);
                }
                locs.insert(iv.value, l);
                active.push((iv.hi, l));
            }
        }
        (locs, used)
    }

    fn slot(&self, v: ValueId) -> Result<Loc, Error> {
        self.slots.get(&v).copied().ok_or_else(|| Error("a value defined outside the function".into()))
    }

    fn get64(&mut self, dst: Gpr, at: Loc) {
        match at {
            Loc::Slot(d) => self.asm.load64(dst, Gpr::Rbp, d),
            Loc::Gpr(r) if r == dst => {}
            Loc::Gpr(r) => self.asm.mov_rr(dst, r),
            Loc::Xmm(x) => self.asm.rr(Some(0x66), true, &[0x0f, 0x7e], x, dst as u8),
        }
    }

    fn put64(&mut self, at: Loc, src: Gpr) {
        match at {
            Loc::Slot(d) => self.asm.store64(Gpr::Rbp, d, src),
            Loc::Gpr(r) if r == src => {}
            Loc::Gpr(r) => self.asm.mov_rr(r, src),
            Loc::Xmm(x) => self.asm.rr(Some(0x66), true, &[0x0f, 0x6e], x, src as u8),
        }
    }

    fn getsd(&mut self, xmm: u8, at: Loc) {
        match at {
            Loc::Slot(d) => self.asm.movsd_load(xmm, Gpr::Rbp, d),
            Loc::Xmm(x) if x == xmm => {}
            Loc::Xmm(x) => self.asm.rr(Some(0x66), false, &[0x0f, 0x28], xmm, x),
            Loc::Gpr(r) => self.asm.rr(Some(0x66), true, &[0x0f, 0x6e], xmm, r as u8),
        }
    }

    fn putsd(&mut self, at: Loc, xmm: u8) {
        match at {
            Loc::Slot(d) => self.asm.movsd_store(Gpr::Rbp, d, xmm),
            Loc::Xmm(x) if x == xmm => {}
            Loc::Xmm(x) => self.asm.rr(Some(0x66), false, &[0x0f, 0x28], x, xmm),
            Loc::Gpr(r) => self.asm.rr(Some(0x66), true, &[0x0f, 0x7e], xmm, r as u8),
        }
    }

    fn epilogue(&mut self) {
        for &(r, at) in &self.saved.clone() {
            self.asm.load64(r, Gpr::Rbp, at);
        }
        self.asm.bytes(&[0xc9, 0xc3]);
    }

    fn kind(&self, v: ValueId) -> Result<Kind, Error> {
        Kind::of(&self.body[v].ty)
    }

    fn function(&mut self, f: OpId) -> Result<(), Error> {
        let body = self.body;
        let region = body[f].regions[0];
        let blocks: Vec<BlockId> = body[region].blocks.iter().copied().collect();
        if self.stencil.is_some() {
            return self.stencil_body(&blocks);
        }
        self.asm.byte(0x55);
        self.asm.mov_rr(Gpr::Rbp, Gpr::Rsp);
        #[cfg(all(windows, target_arch = "x86_64"))]
        if self.frame >= 4096 {
            let mut probe = crate::asm::Asm::default();
            probe.probe_stack(self.frame);
            let _ = probe.finish();
            self.asm.bytes(&probe.bytes);
        }
        if self.frame > 0 {
            self.asm.bytes(&[0x48, 0x81, 0xec]);
            self.asm.bytes(&self.frame.to_le_bytes());
        }
        for &(r, at) in &self.saved.clone() {
            self.asm.store64(Gpr::Rbp, at, r);
        }
        let (mut ints, mut floats) = (0usize, 0u8);
        for &arg in body[blocks[0]].args.iter() {
            let at = self.slot(arg)?;
            match self.kind(arg)? {
                Kind::Int(_) => {
                    let r = *Gpr::ARGS.get(ints).ok_or_else(|| Error("more than six integer parameters".into()))?;
                    self.put64(at, r);
                    ints += 1;
                }
                Kind::F64 if floats < XMM_ARGS => {
                    self.putsd(at, floats);
                    floats += 1;
                }
                Kind::F64 => return Err(Error("more than eight float parameters".into())),
            }
        }
        for &block in &blocks {
            self.blocks.insert(block, self.asm.here());
            for op in body.block_ops(block) {
                self.op(op)?;
            }
        }
        for (at, block) in core::mem::take(&mut self.fixups) {
            let target = self.blocks[&block];
            self.asm.patch(at, target);
        }
        Ok(())
    }

    fn stencil_body(&mut self, blocks: &[BlockId]) -> Result<(), Error> {
        let body = self.body;
        for &block in blocks {
            self.blocks.insert(block, self.asm.here());
            for op in body.block_ops(block) {
                self.op(op)?;
            }
        }
        for (at, block) in core::mem::take(&mut self.fixups) {
            let target = self.blocks[&block];
            self.asm.patch(at, target);
        }
        let mut ends = core::mem::take(&mut self.stencil.as_mut().expect("a stencil").ends);
        if let Some(last) = ends.iter().position(|&at| at + 4 == self.asm.here()) {
            let at = ends.swap_remove(last);
            self.asm.bytes.truncate(at - 1);
        }
        let end = self.asm.here();
        for at in ends {
            self.asm.patch(at, end);
        }
        Ok(())
    }

    fn frame_slot(&self, v: ValueId) -> Option<String> {
        self.stencil.as_ref().and_then(|st| st.slots.get(&v).cloned())
    }

    fn hole(&mut self, kind: HoleKind, name: String) {
        let width = if kind == HoleKind::Abs64 { 8 } else { 4 };
        let at = self.asm.here() - width;
        if let Some(st) = self.stencil.as_mut() {
            st.holes.push(Hole { at, kind, name });
        }
    }

    fn operand(&self, op: OpId, i: usize) -> Result<ValueId, Error> {
        self.body[op].operands.get(i).copied().ok_or_else(|| Error(format!("{} needs operand {i}", self.body[op].name)))
    }

    fn result(&self, op: OpId) -> Result<(ValueId, Loc), Error> {
        let r = *self.body[op].results.first().ok_or_else(|| Error(format!("{} has no result", self.body[op].name)))?;
        Ok((r, self.slot(r)?))
    }

    fn int(&mut self, dst: Gpr, v: ValueId, signed: bool) -> Result<u32, Error> {
        let Kind::Int(bits) = self.kind(v)? else { return Err(Error("an integer operand is a float".into())) };
        let at = self.slot(v)?;
        self.get64(dst, at);
        if signed && bits < 64 {
            let k = u8::try_from(64 - bits).unwrap_or(0);
            self.asm.rr(None, true, &[0xc1], 4, dst as u8);
            self.asm.byte(k);
            self.asm.rr(None, true, &[0xc1], 7, dst as u8);
            self.asm.byte(k);
        }
        Ok(bits)
    }

    fn put_int(&mut self, at: Loc, bits: u32) {
        match bits {
            1 => self.asm.bytes(&[0x83, 0xe0, 0x01]),
            8 => self.asm.bytes(&[0x0f, 0xb6, 0xc0]),
            16 => self.asm.bytes(&[0x0f, 0xb7, 0xc0]),
            32 => self.asm.bytes(&[0x89, 0xc0]),
            _ => {}
        }
        self.put64(at, Gpr::Rax);
    }

    fn op(&mut self, op: OpId) -> Result<(), Error> {
        let body = self.body;
        if let Some(&r) = body[op].results.first()
            && self.frame_slot(r).is_some()
        {
            return Ok(());
        }
        let name: &str = &body[op].name;
        match name {
            "llvm.mlir.constant" => self.constant(op),
            "llvm.mlir.zero" | "llvm.mlir.undef" | "llvm.mlir.poison" => {
                let (_, at) = self.result(op)?;
                self.asm.bytes(&[0x31, 0xc0]);
                self.put64(at, Gpr::Rax);
                Ok(())
            }
            "llvm.add" | "llvm.sub" | "llvm.mul" | "llvm.and" | "llvm.or" | "llvm.xor" => self.int_binary(op, name),
            "llvm.shl" | "llvm.lshr" | "llvm.ashr" => self.shift(op, name),
            "llvm.sdiv" | "llvm.udiv" | "llvm.srem" | "llvm.urem" => self.divide(op, name),
            "llvm.icmp" => self.icmp(op),
            "llvm.fadd" | "llvm.fsub" | "llvm.fmul" | "llvm.fdiv" => self.float_binary(op, name),
            "llvm.fneg" => self.fneg(op),
            "llvm.fcmp" => self.fcmp(op),
            "llvm.select" => self.select(op),
            "llvm.zext" | "llvm.sext" | "llvm.trunc" | "llvm.ptrtoint" | "llvm.inttoptr" => self.int_cast(op, name == "llvm.sext"),
            "llvm.sitofp" | "llvm.uitofp" => self.int_to_float(op),
            "llvm.fptosi" | "llvm.fptoui" => self.float_to_int(op),
            "llvm.bitcast" => self.copy(op),
            "llvm.load" => self.load(op),
            "llvm.store" => self.store(op),
            "llvm.getelementptr" => self.gep(op),
            "llvm.call" => self.call(op),
            "llvm.intr.stacksave" => {
                let (_, at) = self.result(op)?;
                self.put64(at, Gpr::Rsp);
                Ok(())
            }
            "llvm.br" => self.branch(op),
            "llvm.cond_br" => self.cond_branch(op),
            "llvm.return" => self.ret(op),
            "llvm.unreachable" => {
                if self.stencil.is_none() {
                    self.asm.bytes(&[0x0f, 0x0b]);
                }
                Ok(())
            }
            other => Err(Error(format!("{other} is not supported"))),
        }
    }

    fn constant(&mut self, op: OpId) -> Result<(), Error> {
        let (r, at) = self.result(op)?;
        let value = self.body[op].get("value").ok_or_else(|| Error("llvm.mlir.constant without a value".into()))?;
        let bits: u64 = match (value, self.kind(r)?) {
            (Attribute::Bool(b), _) => u64::from(*b),
            (Attribute::Float { lit, .. }, Kind::F64) => match lit.strip_prefix("0x").or_else(|| lit.strip_prefix("0X")) {
                Some(h) => u64::from_str_radix(h, 16).map_err(|_| Error(format!("bad float {lit}")))?,
                None => lit.parse::<f64>().map_err(|_| Error(format!("bad float {lit}")))?.to_bits(),
            },
            (Attribute::Int { lit, .. }, _) => match lit.strip_prefix("0x") {
                Some(h) => u64::from_str_radix(h, 16).map_err(|_| Error(format!("bad integer {lit}")))?,
                None => Attrs::as_int(value)?.cast_unsigned(),
            },
            (a, _) => return Err(Error(format!("constant {a} is not supported"))),
        };
        let bits = match self.kind(r)? {
            Kind::Int(w) if w < 64 => bits & ((1u64 << w) - 1),
            _ => bits,
        };
        self.asm.mov_imm64(Gpr::Rax, bits);
        self.put64(at, Gpr::Rax);
        Ok(())
    }

    fn int_binary(&mut self, op: OpId, name: &str) -> Result<(), Error> {
        let (_, at) = self.result(op)?;
        let bits = self.int(Gpr::Rax, self.operand(op, 0)?, false)?;
        self.int(Gpr::Rcx, self.operand(op, 1)?, false)?;
        let w = bits > 32;
        match name {
            "llvm.add" => self.asm.rr(None, w, &[0x01], Gpr::Rcx as u8, Gpr::Rax as u8),
            "llvm.sub" => self.asm.rr(None, w, &[0x29], Gpr::Rcx as u8, Gpr::Rax as u8),
            "llvm.and" => self.asm.rr(None, w, &[0x21], Gpr::Rcx as u8, Gpr::Rax as u8),
            "llvm.or" => self.asm.rr(None, w, &[0x09], Gpr::Rcx as u8, Gpr::Rax as u8),
            "llvm.xor" => self.asm.rr(None, w, &[0x31], Gpr::Rcx as u8, Gpr::Rax as u8),
            _ => self.asm.rr(None, w, &[0x0f, 0xaf], Gpr::Rax as u8, Gpr::Rcx as u8),
        }
        self.put_int(at, bits);
        Ok(())
    }

    fn shift(&mut self, op: OpId, name: &str) -> Result<(), Error> {
        let (_, at) = self.result(op)?;
        let signed = name == "llvm.ashr";
        let bits = self.int(Gpr::Rax, self.operand(op, 0)?, signed)?;
        self.int(Gpr::Rcx, self.operand(op, 1)?, false)?;
        let ext = match name {
            "llvm.shl" => 4,
            "llvm.lshr" => 5,
            _ => 7,
        };
        self.asm.rr(None, true, &[0xd3], ext, Gpr::Rax as u8);
        self.put_int(at, bits);
        Ok(())
    }

    fn divide(&mut self, op: OpId, name: &str) -> Result<(), Error> {
        let (_, at) = self.result(op)?;
        let signed = matches!(name, "llvm.sdiv" | "llvm.srem");
        let bits = self.int(Gpr::Rax, self.operand(op, 0)?, signed)?;
        self.int(Gpr::Rcx, self.operand(op, 1)?, signed)?;
        if signed {
            self.asm.bytes(&[0x48, 0x99]);
            self.asm.rr(None, true, &[0xf7], 7, Gpr::Rcx as u8);
        } else {
            self.asm.bytes(&[0x31, 0xd2]);
            self.asm.rr(None, true, &[0xf7], 6, Gpr::Rcx as u8);
        }
        if matches!(name, "llvm.srem" | "llvm.urem") {
            self.asm.mov_rr(Gpr::Rax, Gpr::Rdx);
        }
        self.put_int(at, bits);
        Ok(())
    }

    fn icmp(&mut self, op: OpId) -> Result<(), Error> {
        let (_, at) = self.result(op)?;
        let pred = ICmpPredicate::of(self.predicate(op)?)?;
        let signed = matches!(pred, ICmpPredicate::Slt | ICmpPredicate::Sle | ICmpPredicate::Sgt | ICmpPredicate::Sge);
        self.int(Gpr::Rax, self.operand(op, 0)?, signed)?;
        self.int(Gpr::Rcx, self.operand(op, 1)?, signed)?;
        self.asm.rr(None, true, &[0x39], Gpr::Rcx as u8, Gpr::Rax as u8);
        let cc = match pred {
            ICmpPredicate::Eq => Cc::E,
            ICmpPredicate::Ne => Cc::NE,
            ICmpPredicate::Slt => Cc::L,
            ICmpPredicate::Sle => Cc::LE,
            ICmpPredicate::Sgt => Cc::G,
            ICmpPredicate::Sge => Cc::GE,
            ICmpPredicate::Ult => Cc::B,
            ICmpPredicate::Ule => Cc::BE,
            ICmpPredicate::Ugt => Cc::A,
            ICmpPredicate::Uge => Cc::AE,
        };
        self.asm.setcc(cc);
        self.put_int(at, 1);
        Ok(())
    }

    fn predicate(&self, op: OpId) -> Result<i64, Error> {
        match self.body[op].get("predicate") {
            Some(a @ Attribute::Int { .. }) => Attrs::as_int(a),
            _ => Err(Error(format!("{} without a predicate", self.body[op].name))),
        }
    }

    fn floats(&mut self, a: ValueId, b: ValueId) -> Result<(), Error> {
        let (sa, sb) = (self.slot(a)?, self.slot(b)?);
        self.getsd(0, sa);
        self.getsd(1, sb);
        Ok(())
    }

    fn float_binary(&mut self, op: OpId, name: &str) -> Result<(), Error> {
        let (_, at) = self.result(op)?;
        self.floats(self.operand(op, 0)?, self.operand(op, 1)?)?;
        let code = match name {
            "llvm.fadd" => 0x58,
            "llvm.fmul" => 0x59,
            "llvm.fsub" => 0x5c,
            _ => 0x5e,
        };
        self.asm.rr(Some(0xf2), false, &[0x0f, code], 0, 1);
        self.putsd(at, 0);
        Ok(())
    }

    fn fneg(&mut self, op: OpId) -> Result<(), Error> {
        let (_, at) = self.result(op)?;
        let src = self.slot(self.operand(op, 0)?)?;
        self.get64(Gpr::Rax, src);
        self.asm.mov_imm64(Gpr::Rcx, 1 << 63);
        self.asm.rr(None, true, &[0x31], Gpr::Rcx as u8, Gpr::Rax as u8);
        self.put64(at, Gpr::Rax);
        Ok(())
    }

    fn fcmp(&mut self, op: OpId) -> Result<(), Error> {
        let (_, at) = self.result(op)?;
        let pred = FCmpPredicate::of(self.predicate(op)?)?;
        let (a, b) = (self.operand(op, 0)?, self.operand(op, 1)?);
        let (swap, cc, extra) = match pred {
            FCmpPredicate::False => (false, None, None),
            FCmpPredicate::True => (false, None, Some(true)),
            FCmpPredicate::Ogt => (false, Some(Cc::A), None),
            FCmpPredicate::Oge => (false, Some(Cc::AE), None),
            FCmpPredicate::Olt => (true, Some(Cc::A), None),
            FCmpPredicate::Ole => (true, Some(Cc::AE), None),
            FCmpPredicate::Ugt => (true, Some(Cc::B), None),
            FCmpPredicate::Uge => (true, Some(Cc::BE), None),
            FCmpPredicate::Ult => (false, Some(Cc::B), None),
            FCmpPredicate::Ule => (false, Some(Cc::BE), None),
            FCmpPredicate::One => (false, Some(Cc::NE), None),
            FCmpPredicate::Ueq => (false, Some(Cc::E), None),
            FCmpPredicate::Ord => (false, Some(Cc::NP), None),
            FCmpPredicate::Uno => (false, Some(Cc::P), None),
            FCmpPredicate::Oeq | FCmpPredicate::Une => (false, None, None),
        };
        if let Some(c) = extra {
            self.asm.mov_imm64(Gpr::Rax, u64::from(c));
            self.put64(at, Gpr::Rax);
            return Ok(());
        }
        let (x, y) = if swap { (b, a) } else { (a, b) };
        self.floats(x, y)?;
        self.asm.rr(Some(0x66), false, &[0x0f, 0x2e], 0, 1);
        match (pred, cc) {
            (FCmpPredicate::Oeq, _) => {
                self.asm.bytes(&[0x0f, 0x94, 0xc0, 0x0f, 0x9b, 0xc1, 0x20, 0xc8, 0x0f, 0xb6, 0xc0]);
            }
            (FCmpPredicate::Une, _) => {
                self.asm.bytes(&[0x0f, 0x95, 0xc0, 0x0f, 0x9a, 0xc1, 0x08, 0xc8, 0x0f, 0xb6, 0xc0]);
            }
            (_, Some(cc)) => self.asm.setcc(cc),
            (_, None) => self.asm.bytes(&[0x31, 0xc0]),
        }
        self.put_int(at, 1);
        Ok(())
    }

    fn select(&mut self, op: OpId) -> Result<(), Error> {
        let (_, at) = self.result(op)?;
        let (c, t, f) = (self.slot(self.operand(op, 0)?)?, self.slot(self.operand(op, 1)?)?, self.slot(self.operand(op, 2)?)?);
        self.get64(Gpr::Rax, f);
        self.get64(Gpr::Rcx, t);
        self.get64(Gpr::Rdx, c);
        self.asm.bytes(&[0x85, 0xd2]);
        self.asm.rr(None, true, &[0x0f, 0x45], Gpr::Rax as u8, Gpr::Rcx as u8);
        self.put64(at, Gpr::Rax);
        Ok(())
    }

    fn int_cast(&mut self, op: OpId, signed: bool) -> Result<(), Error> {
        let (r, at) = self.result(op)?;
        self.int(Gpr::Rax, self.operand(op, 0)?, signed)?;
        let Kind::Int(bits) = self.kind(r)? else { return Err(Error("an integer cast to a float".into())) };
        self.put_int(at, bits);
        Ok(())
    }

    fn copy(&mut self, op: OpId) -> Result<(), Error> {
        let (_, at) = self.result(op)?;
        let src = self.slot(self.operand(op, 0)?)?;
        self.get64(Gpr::Rax, src);
        self.put64(at, Gpr::Rax);
        Ok(())
    }

    fn int_to_float(&mut self, op: OpId) -> Result<(), Error> {
        let (_, at) = self.result(op)?;
        let signed = self.body[op].name == "llvm.sitofp";
        self.int(Gpr::Rax, self.operand(op, 0)?, signed)?;
        self.asm.rr(Some(0xf2), true, &[0x0f, 0x2a], 0, Gpr::Rax as u8);
        self.putsd(at, 0);
        Ok(())
    }

    fn float_to_int(&mut self, op: OpId) -> Result<(), Error> {
        let (r, at) = self.result(op)?;
        let src = self.slot(self.operand(op, 0)?)?;
        self.getsd(0, src);
        self.asm.rr(Some(0xf2), true, &[0x0f, 0x2c], Gpr::Rax as u8, 0);
        let Kind::Int(bits) = self.kind(r)? else { return Err(Error("a float cast to a float".into())) };
        self.put_int(at, bits);
        Ok(())
    }

    fn load(&mut self, op: OpId) -> Result<(), Error> {
        let (r, at) = self.result(op)?;
        let addr = self.operand(op, 0)?;
        let frame = self.frame_slot(addr);
        let base = match &frame {
            Some(_) => Gpr::Rbp,
            None => {
                let ptr = self.slot(addr)?;
                self.get64(Gpr::Rcx, ptr);
                Gpr::Rcx
            }
        };
        match self.kind(r)? {
            Kind::Int(bits) => self.asm.load_int(Gpr::Rax, base, 0, bits.max(8).next_power_of_two(), false),
            Kind::F64 => self.asm.load64(Gpr::Rax, base, 0),
        }
        if let Some(name) = frame {
            self.hole(HoleKind::Disp32, name);
        }
        self.put64(at, Gpr::Rax);
        Ok(())
    }

    fn store(&mut self, op: OpId) -> Result<(), Error> {
        let (v, p) = (self.operand(op, 0)?, self.operand(op, 1)?);
        let bytes = Kind::size(&self.body[v].ty)?;
        let sv = self.slot(v)?;
        self.get64(Gpr::Rax, sv);
        match self.frame_slot(p) {
            Some(name) => {
                self.asm.store_int(Gpr::Rbp, 0, Gpr::Rax, bytes);
                self.hole(HoleKind::Disp32, name);
            }
            None => {
                let sp = self.slot(p)?;
                self.get64(Gpr::Rcx, sp);
                self.asm.store_int(Gpr::Rcx, 0, Gpr::Rax, bytes);
            }
        }
        Ok(())
    }

    fn gep(&mut self, op: OpId) -> Result<(), Error> {
        let (_, at) = self.result(op)?;
        let elem = match self.body[op].get("elem_type") {
            Some(Attribute::Type(t)) => t.clone(),
            _ => return Err(Error("getelementptr without an element type".into())),
        };
        let size = i64::from(Kind::size(&elem).map_err(|_| Error(format!("getelementptr over {elem} is not supported")))?);
        let raw: Vec<i64> = match self.body[op].get("rawConstantIndices") {
            Some(Attribute::DenseArray { values, .. }) => values.iter().map(|v| v.parse::<i64>().map_err(|_| Error(format!("bad index {v}")))).collect::<Result<_, _>>()?,
            _ => return Err(Error("getelementptr without indices".into())),
        };
        let [index] = raw[..] else { return Err(Error("getelementptr with more than one index is not supported".into())) };
        let base = self.slot(self.operand(op, 0)?)?;
        self.get64(Gpr::Rax, base);
        const DYNAMIC: i64 = i32::MIN as i64;
        if index == DYNAMIC {
            self.int(Gpr::Rcx, self.operand(op, 1)?, true)?;
            self.asm.rr(None, true, &[0x69], Gpr::Rcx as u8, Gpr::Rcx as u8);
            self.asm.bytes(&i32::try_from(size).unwrap_or(1).to_le_bytes());
        } else {
            self.asm.mov_imm64(Gpr::Rcx, (index * size).cast_unsigned());
        }
        self.asm.rr(None, true, &[0x01], Gpr::Rcx as u8, Gpr::Rax as u8);
        self.put64(at, Gpr::Rax);
        Ok(())
    }

    fn call(&mut self, op: OpId) -> Result<(), Error> {
        let callee = match self.body[op].get("callee") {
            Some(a) => Attrs::as_symbol(a)?.to_string(),
            None => return Err(Error("an indirect call is not supported".into())),
        };
        if self.stencil.is_some() {
            if callee.starts_with(Stenciled::SLOT) {
                return Ok(());
            }
            if callee.starts_with(Stenciled::JUMP) {
                self.asm.jump(None);
                self.hole(HoleKind::Rel32, callee);
                return Ok(());
            }
            if callee.starts_with(Stenciled::HOLE) {
                let (_, at) = self.result(op)?;
                self.asm.mov_imm64(Gpr::Rax, 0);
                self.hole(HoleKind::Abs64, callee);
                self.put64(at, Gpr::Rax);
                return Ok(());
            }
        }
        let (mut ints, mut floats) = (0usize, 0u8);
        let args: Vec<ValueId> = self.body[op].operands.iter().copied().collect();
        for v in args {
            let at = self.slot(v)?;
            match self.kind(v)? {
                Kind::Int(_) => {
                    let r = *Gpr::ARGS.get(ints).ok_or_else(|| Error("a call with more than six integer arguments".into()))?;
                    self.get64(r, at);
                    ints += 1;
                }
                Kind::F64 if floats < XMM_ARGS => {
                    self.getsd(floats, at);
                    floats += 1;
                }
                Kind::F64 => return Err(Error("a call with more than eight float arguments".into())),
            }
        }
        let offset = self.asm.mov_imm64(Gpr::Rax, 0);
        if self.stencil.is_some() {
            self.hole(HoleKind::Abs64, callee);
        } else {
            self.relocs.push(X86_64Reloc { offset, symbol: callee });
        }
        self.asm.bytes(&[0xff, 0xd0]);
        if let Some(&r) = self.body[op].results.first() {
            let at = self.slot(r)?;
            match self.kind(r)? {
                Kind::Int(bits) => self.put_int(at, bits),
                Kind::F64 => self.putsd(at, 0),
            }
        }
        Ok(())
    }

    fn pass(&mut self, values: &[ValueId], to: BlockId) -> Result<(), Error> {
        let args: Vec<ValueId> = self.body[to].args.iter().copied().collect();
        if self.stencil.is_some() && !values.is_empty() {
            return Err(Error("a stencil passes no block arguments".into()));
        }
        if args.len() != values.len() {
            return Err(Error("a jump with the wrong number of block arguments".into()));
        }
        for (i, &v) in (0i32..).zip(values) {
            let s = self.slot(v)?;
            self.get64(Gpr::Rax, s);
            self.asm.store64(Gpr::Rbp, self.scratch + 8 * i, Gpr::Rax);
        }
        for (i, &a) in (0i32..).zip(&args) {
            let d = self.slot(a)?;
            self.asm.load64(Gpr::Rax, Gpr::Rbp, self.scratch + 8 * i);
            self.put64(d, Gpr::Rax);
        }
        Ok(())
    }

    fn jump_to(&mut self, cc: Option<u8>, to: BlockId) {
        let at = self.asm.jump(cc);
        self.fixups.push((at, to));
    }

    fn branch(&mut self, op: OpId) -> Result<(), Error> {
        let to = *self.body[op].successors.first().ok_or_else(|| Error("llvm.br without a successor".into()))?;
        let values: Vec<ValueId> = self.body[op].operands.iter().copied().collect();
        self.pass(&values, to)?;
        self.jump_to(None, to);
        Ok(())
    }

    fn cond_branch(&mut self, op: OpId) -> Result<(), Error> {
        let succ: Vec<BlockId> = self.body[op].successors.iter().copied().collect();
        let [t, f] = succ[..] else { return Err(Error("llvm.cond_br needs two successors".into())) };
        let sizes: Vec<usize> = match self.body[op].get("operandSegmentSizes") {
            Some(Attribute::DenseArray { values, .. }) => values.iter().map(|v| v.parse::<usize>().unwrap_or(0)).collect(),
            _ => std::vec![1, 0, 0],
        };
        let ops: Vec<ValueId> = self.body[op].operands.iter().copied().collect();
        let (nt, nf) = (sizes.get(1).copied().unwrap_or(0), sizes.get(2).copied().unwrap_or(0));
        let cond = self.slot(ops[0])?;
        self.get64(Gpr::Rax, cond);
        self.asm.bytes(&[0x85, 0xc0]);
        let to_false = self.asm.jump(Some(Cc::E));
        self.pass(&ops[1..=nt], t)?;
        self.jump_to(None, t);
        let here = self.asm.here();
        self.asm.patch(to_false, here);
        self.pass(&ops[1 + nt..1 + nt + nf], f)?;
        self.jump_to(None, f);
        Ok(())
    }

    fn ret(&mut self, op: OpId) -> Result<(), Error> {
        if let Some(st) = self.stencil.as_mut() {
            if !self.body[op].operands.is_empty() {
                return Err(Error("a stencil returns nothing".into()));
            }
            st.ends.push(self.asm.jump(None));
            return Ok(());
        }
        if let Some(&v) = self.body[op].operands.first() {
            let at = self.slot(v)?;
            match self.kind(v)? {
                Kind::Int(_) => self.get64(Gpr::Rax, at),
                Kind::F64 => self.getsd(0, at),
            }
        }
        self.epilogue();
        Ok(())
    }
}

impl From<X86_64Code> for crate::MachineCode {
    fn from(c: X86_64Code) -> Self {
        crate::MachineCode { name: c.name, bytes: c.bytes, relocs: c.relocs.into_iter().map(|r| (r.offset, r.symbol)).collect() }
    }
}

impl StencilEmit for X86_64Emit {
    fn stencils(&self, body: &Body<'_>, root: OpId) -> Result<Vec<Stencil>, Error> {
        let mut out = Vec::new();
        for &region in body[root].regions.iter() {
            for &block in body[region].blocks.iter() {
                for f in body.block_ops(block) {
                    if body[f].name != "llvm.func" || !body[f].has_flag(Stenciled::ATTR) {
                        continue;
                    }
                    let name = body[f].get_str("sym_name").ok_or_else(|| Error("x86-64: a stencil without a name".into()))?.to_string();
                    let mut lower = Lower::new(body, f).map_err(|e| Error(format!("x86-64: stencil @{name}: {}", e.0)))?;
                    lower.function(f).map_err(|e| Error(format!("x86-64: stencil @{name}: {}", e.0)))?;
                    let holes = lower.stencil.map(|st| st.holes).unwrap_or_default();
                    out.push(Stencil { name, code: lower.asm.bytes, holes });
                }
            }
        }
        Ok(out)
    }
}

impl crate::Backend for X86_64Emit {
    fn emit(&self, body: &Body<'_>, root: OpId) -> Result<Vec<crate::MachineCode>, Error> {
        Emit::emit(self, body, root).map(|codes| codes.into_iter().map(Into::into).collect())
    }
}

trait Predicate: Sized + Copy + 'static {
    const ALL: &'static [Self];

    fn of(i: i64) -> Result<Self, Error> {
        usize::try_from(i).ok().and_then(|i| Self::ALL.get(i)).copied().ok_or_else(|| Error(format!("predicate {i} out of range")))
    }
}

impl Predicate for ICmpPredicate {
    const ALL: &'static [Self] = &[ICmpPredicate::Eq, ICmpPredicate::Ne, ICmpPredicate::Slt, ICmpPredicate::Sle, ICmpPredicate::Sgt, ICmpPredicate::Sge, ICmpPredicate::Ult, ICmpPredicate::Ule, ICmpPredicate::Ugt, ICmpPredicate::Uge];
}

impl Predicate for FCmpPredicate {
    const ALL: &'static [Self] = &[FCmpPredicate::False, FCmpPredicate::Oeq, FCmpPredicate::Ogt, FCmpPredicate::Oge, FCmpPredicate::Olt, FCmpPredicate::Ole, FCmpPredicate::One, FCmpPredicate::Ord, FCmpPredicate::Ueq, FCmpPredicate::Ugt, FCmpPredicate::Uge, FCmpPredicate::Ult, FCmpPredicate::Ule, FCmpPredicate::Une, FCmpPredicate::Uno, FCmpPredicate::True];
}
