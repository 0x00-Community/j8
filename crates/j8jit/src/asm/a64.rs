use alloc::vec::Vec;

use super::{Alu, Cc, Label, Shift, R, RAX, RBP, RDX, RSP, SAVED, SAVED_END, X};
#[cfg(windows)]
use super::R11;
use crate::ir::FloatOp;

type H = u32;

const HW: [H; 16] = [9, 3, 2, 19, 31, 29, 1, 0, 4, 5, 10, 11, 20, 21, 22, 23];
const SP: H = 31;
const ZR: H = 31;
const FP: H = 29;
const LR: H = 30;
const T0: H = 16;
const T1: H = 17;
const RET0: H = 0;
const RET1: H = 1;
const FLOAT_BASE: H = 16;
const IMM12: i64 = 1 << 12;
const IMM9: i64 = 1 << 8;
const SLOT: i32 = 16;

#[derive(Clone, Copy, PartialEq, Eq)]
enum Flags {
    Int,
    Float,
    Mul,
}

#[derive(Clone, Copy)]
enum Fix {
    Cond,
    Jump,
}

#[derive(Clone, Copy)]
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
enum Mem {
    X,
    W,
    B,
    D,
}

impl Mem {
    fn size(self) -> i32 {
        match self {
            Mem::X | Mem::D => 8,
            Mem::W => 4,
            Mem::B => 1,
        }
    }

    fn scaled(self, load: bool) -> u32 {
        match (self, load) {
            (Mem::X, true) => 0xF940_0000,
            (Mem::X, false) => 0xF900_0000,
            (Mem::W, true) => 0xB940_0000,
            (Mem::W, false) => 0xB900_0000,
            (Mem::B, true) => 0x3940_0000,
            (Mem::B, false) => 0x3900_0000,
            (Mem::D, true) => 0xFD40_0000,
            (Mem::D, false) => 0xFD00_0000,
        }
    }

    fn unscaled(self, load: bool) -> u32 {
        match (self, load) {
            (Mem::X, true) => 0xF840_0000,
            (Mem::X, false) => 0xF800_0000,
            (Mem::W, true) => 0xB840_0000,
            (Mem::W, false) => 0xB800_0000,
            (Mem::B, true) => 0x3840_0000,
            (Mem::B, false) => 0x3800_0000,
            (Mem::D, true) => 0xFC40_0000,
            (Mem::D, false) => 0xFC00_0000,
        }
    }

    fn indexed(self, load: bool) -> u32 {
        match (self, load) {
            (Mem::X, true) => 0xF860_6800,
            (Mem::X, false) => 0xF820_6800,
            (Mem::W, true) => 0xB860_6800,
            (Mem::W, false) => 0xB820_6800,
            (Mem::B, true) => 0x3860_6800,
            (Mem::B, false) => 0x3820_6800,
            (Mem::D, true) => 0xFC60_6800,
            (Mem::D, false) => 0xFC20_6800,
        }
    }
}

pub struct Asm {
    pub bytes: Vec<u8>,
    labels: Vec<Option<usize>>,
    fixups: Vec<(usize, Label, Fix)>,
    flags: Flags,
}

impl Default for Asm {
    fn default() -> Self {
        Self { bytes: Vec::new(), labels: Vec::new(), fixups: Vec::new(), flags: Flags::Int }
    }
}

fn hw(r: R) -> H {
    HW[usize::from(r)]
}

fn fx(x: X) -> H {
    FLOAT_BASE + H::from(x)
}

fn sf(w: bool) -> u32 {
    u32::from(w) << 31
}

fn field(v: i64, bits: u32) -> u32 {
    u32::try_from(v & ((1 << bits) - 1)).unwrap_or(0)
}

impl Asm {
    pub fn label(&mut self) -> Label {
        self.labels.push(None);
        u32::try_from(self.labels.len() - 1).unwrap_or(0)
    }

    pub fn bind(&mut self, l: Label) {
        self.labels[l as usize] = Some(self.bytes.len());
    }

    pub fn at(&self, l: Label) -> Option<usize> {
        self.labels.get(l as usize).copied().flatten()
    }

    pub fn here(&self) -> usize {
        self.bytes.len()
    }

    pub fn finish(&mut self) -> bool {
        for &(at, l, fix) in &self.fixups {
            let Some(to) = self.labels[l as usize] else { return false };
            let rel = (i64::try_from(to).unwrap_or(0) - i64::try_from(at).unwrap_or(0)) / 4;
            let mut word = u32::from_le_bytes([self.bytes[at], self.bytes[at + 1], self.bytes[at + 2], self.bytes[at + 3]]);
            word |= match fix {
                Fix::Cond => field(rel, 19) << 5,
                Fix::Jump => field(rel, 26),
            };
            self.bytes[at..at + 4].copy_from_slice(&word.to_le_bytes());
        }
        true
    }

    fn put(&mut self, word: u32) {
        self.bytes.extend_from_slice(&word.to_le_bytes());
    }

    fn imm(&mut self, d: H, v: u64) {
        let chunks: [u64; 4] = core::array::from_fn(|i| v >> (16 * i) & 0xFFFF);
        let ones = chunks.iter().filter(|&&c| c == 0xFFFF).count();
        let zeros = chunks.iter().filter(|&&c| c == 0).count();
        let (skip, first): (u64, u32) = if ones > zeros { (0xFFFF, 0x9280_0000) } else { (0, 0xD280_0000) };
        let mut placed = false;
        for (i, &c) in chunks.iter().enumerate() {
            if c == skip {
                continue;
            }
            let hw16 = u32::try_from(i).unwrap_or(0) << 21;
            let c16 = u32::try_from(c).unwrap_or(0);
            if placed {
                self.put(0xF280_0000 | hw16 | c16 << 5 | d);
            } else {
                let c16 = if skip == 0 { c16 } else { !c16 & 0xFFFF };
                self.put(first | hw16 | c16 << 5 | d);
                placed = true;
            }
        }
        if !placed {
            self.put(first | d);
        }
    }

    fn orr(&mut self, w: bool, d: H, n: H, m: H) {
        self.put(sf(w) | 0x2A00_0000 | m << 16 | n << 5 | d);
    }

    fn mov(&mut self, d: H, s: H) {
        if d == s {
            return;
        }
        if d == SP || s == SP {
            self.put(0x9100_0000 | s << 5 | d);
        } else {
            self.orr(true, d, ZR, s);
        }
    }

    fn add_ext(&mut self, d: H, n: H, m: H) {
        self.put(0x8B20_6000 | m << 16 | n << 5 | d);
    }

    fn add_imm(&mut self, d: H, n: H, v: i64) {
        match v {
            0..IMM12 => self.put(0x9100_0000 | field(v, 12) << 10 | n << 5 | d),
            _ if (-IMM12 + 1..0).contains(&v) => self.put(0xD100_0000 | field(-v, 12) << 10 | n << 5 | d),
            _ => {
                self.imm(T1, v.cast_unsigned());
                self.add_ext(d, n, T1);
            }
        }
    }

    fn mem(&mut self, kind: Mem, load: bool, t: H, base: H, disp: i32) {
        let size = kind.size();
        let d = i64::from(disp);
        if disp >= 0 && disp % size == 0 && d / i64::from(size) < IMM12 {
            self.put(kind.scaled(load) | field(d / i64::from(size), 12) << 10 | base << 5 | t);
        } else if (-IMM9..IMM9).contains(&d) {
            self.put(kind.unscaled(load) | field(d, 9) << 12 | base << 5 | t);
        } else {
            self.imm(T1, d.cast_unsigned());
            self.put(kind.indexed(load) | T1 << 16 | base << 5 | t);
        }
    }

    fn alu(&mut self, op: Alu, w: bool, d: H, n: H, m: H) {
        let s = sf(w);
        match op {
            Alu::Add => self.put(s | 0x2B00_0000 | m << 16 | n << 5 | d),
            Alu::Sub => self.put(s | 0x6B00_0000 | m << 16 | n << 5 | d),
            Alu::Cmp => self.put(s | 0x6B00_0000 | m << 16 | n << 5 | ZR),
            Alu::And => self.put(s | 0x6A00_0000 | m << 16 | n << 5 | d),
            Alu::Or | Alu::Xor => {
                let code = if op == Alu::Or { 0x2A00_0000 } else { 0x4A00_0000 };
                self.put(s | code | m << 16 | n << 5 | d);
                self.put(s | 0x6A00_0000 | d << 16 | d << 5 | ZR);
            }
        }
        self.flags = Flags::Int;
    }

    fn cond(&self, cc: Cc) -> Cond {
        let cc = match (self.flags, cc) {
            (Flags::Mul, Cc::O) => Cc::Ne,
            (Flags::Mul, Cc::No) => Cc::E,
            (Flags::Float, Cc::A) => Cc::G,
            (Flags::Float, Cc::Ae) => Cc::Ge,
            (Flags::Float, Cc::B) => Cc::L,
            (Flags::Float, Cc::Be) => Cc::Le,
            (_, cc) => cc,
        };
        match cc {
            Cc::O | Cc::P => Cond::Vs,
            Cc::No | Cc::Np => Cond::Vc,
            Cc::B => Cond::Lo,
            Cc::Ae => Cond::Hs,
            Cc::E => Cond::Eq,
            Cc::Ne => Cond::Ne,
            Cc::Be => Cond::Ls,
            Cc::A => Cond::Hi,
            Cc::S => Cond::Mi,
            Cc::Ns => Cond::Pl,
            Cc::L => Cond::Lt,
            Cc::Ge => Cond::Ge,
            Cc::Le => Cond::Le,
            Cc::G => Cond::Gt,
        }
    }

    pub fn mov_rr(&mut self, dst: R, src: R) {
        self.mov(hw(dst), hw(src));
    }

    pub fn mov_rr32(&mut self, dst: R, src: R) {
        self.orr(false, hw(dst), ZR, hw(src));
    }

    pub fn mov_ri(&mut self, dst: R, imm: u64) {
        self.imm(hw(dst), imm);
    }

    pub fn load(&mut self, dst: R, base: R, disp: i32) {
        self.mem(Mem::X, true, hw(dst), hw(base), disp);
    }

    pub fn load32(&mut self, dst: R, base: R, disp: i32) {
        self.mem(Mem::W, true, hw(dst), hw(base), disp);
    }

    pub fn store(&mut self, base: R, disp: i32, src: R) {
        self.mem(Mem::X, false, hw(src), hw(base), disp);
    }

    pub fn store32(&mut self, base: R, disp: i32, src: R) {
        self.mem(Mem::W, false, hw(src), hw(base), disp);
    }

    pub fn lea(&mut self, dst: R, base: R, disp: i32) {
        self.add_imm(hw(dst), hw(base), i64::from(disp));
    }

    pub fn alu_rr(&mut self, op: Alu, w: bool, dst: R, src: R) {
        self.alu(op, w, hw(dst), hw(dst), hw(src));
    }

    pub fn alu_ri(&mut self, op: Alu, w: bool, dst: R, imm: i32) {
        let d = hw(dst);
        let v = i64::from(imm);
        if dst == RSP {
            self.add_imm(SP, SP, if op == Alu::Sub { -v } else { v });
            return;
        }
        match op {
            Alu::Add | Alu::Sub | Alu::Cmp if (0..IMM12).contains(&v) => {
                let code = if op == Alu::Add { 0x3100_0000 } else { 0x7100_0000 };
                let rd = if op == Alu::Cmp { ZR } else { d };
                self.put(sf(w) | code | field(v, 12) << 10 | d << 5 | rd);
                self.flags = Flags::Int;
            }
            _ => {
                let bits = if w { v.cast_unsigned() } else { u64::from(imm.cast_unsigned()) };
                self.imm(T1, bits);
                self.alu(op, w, d, d, T1);
            }
        }
    }

    pub fn alu_rm(&mut self, op: Alu, w: bool, dst: R, base: R, disp: i32) {
        self.mem(if w { Mem::X } else { Mem::W }, true, T0, hw(base), disp);
        if dst == RSP {
            self.put(0xEB20_6000 | T0 << 16 | SP << 5 | ZR);
            self.flags = Flags::Int;
            return;
        }
        self.alu(op, w, hw(dst), hw(dst), T0);
    }

    pub fn cmp_mi(&mut self, w: bool, base: R, disp: i32, imm: i32) {
        self.mem(if w { Mem::X } else { Mem::W }, true, T0, hw(base), disp);
        let bits = if w { i64::from(imm).cast_unsigned() } else { u64::from(imm.cast_unsigned()) };
        self.imm(T1, bits);
        self.alu(Alu::Cmp, w, ZR, T0, T1);
    }

    pub fn cmp_m8i(&mut self, base: R, disp: i32, imm: u8) {
        self.mem(Mem::B, true, T0, hw(base), disp);
        self.put(0x7100_0000 | u32::from(imm) << 10 | T0 << 5 | ZR);
        self.flags = Flags::Int;
    }

    pub fn test_m8i(&mut self, base: R, disp: i32, imm: u8) {
        self.mem(Mem::B, true, T0, hw(base), disp);
        self.imm(T1, u64::from(imm));
        self.alu(Alu::And, false, ZR, T0, T1);
    }

    pub fn test_rr(&mut self, w: bool, a: R, b: R) {
        self.alu(Alu::And, w, ZR, hw(a), hw(b));
    }

    fn mul(&mut self, w: bool, d: H, n: H, m: H) {
        if w {
            self.put(0x9B00_7C00 | m << 16 | n << 5 | d);
            return;
        }
        self.put(0x9B20_7C00 | m << 16 | n << 5 | T0);
        self.put(0xEB20_C000 | T0 << 16 | T0 << 5 | ZR);
        self.orr(false, d, ZR, T0);
        self.flags = Flags::Mul;
    }

    pub fn imul_rr(&mut self, w: bool, dst: R, src: R) {
        self.mul(w, hw(dst), hw(dst), hw(src));
    }

    pub fn imul_rri(&mut self, w: bool, dst: R, src: R, imm: i32) {
        self.imm(T1, i64::from(imm).cast_unsigned());
        self.mul(w, hw(dst), hw(src), T1);
    }

    fn divide(&mut self, w: bool, src: H) {
        let (q, r) = (hw(RAX), hw(RDX));
        self.put(sf(w) | 0x1AC0_0C00 | src << 16 | q << 5 | T0);
        self.put(sf(w) | 0x1B00_8000 | src << 16 | q << 10 | T0 << 5 | r);
        self.orr(w, q, ZR, T0);
    }

    pub fn cdq_idiv(&mut self, src: R) {
        self.divide(false, hw(src));
    }

    pub fn cqo_idiv(&mut self, src: R) {
        self.divide(true, hw(src));
    }

    pub fn shift_cl(&mut self, kind: Shift, w: bool, dst: R) {
        let code = match kind {
            Shift::Shl => 0x1AC0_2000,
            Shift::Shr => 0x1AC0_2400,
            Shift::Sar => 0x1AC0_2800,
        };
        let d = hw(dst);
        self.put(sf(w) | code | hw(super::RCX) << 16 | d << 5 | d);
    }

    fn bitfield(&mut self, signed: bool, w: bool, d: H, n: H, immr: u32, imms: u32) {
        let base = match (signed, w) {
            (false, false) => 0x5300_0000,
            (false, true) => 0xD340_0000,
            (true, false) => 0x1300_0000,
            (true, true) => 0x9340_0000,
        };
        self.put(base | immr << 16 | imms << 10 | n << 5 | d);
    }

    pub fn shift_ri(&mut self, kind: Shift, w: bool, dst: R, n: u8) {
        let size: u32 = if w { 64 } else { 32 };
        let n = u32::from(n) % size;
        let d = hw(dst);
        match kind {
            Shift::Shl => self.bitfield(false, w, d, d, (size - n) % size, size - 1 - n),
            Shift::Shr => self.bitfield(false, w, d, d, n, size - 1),
            Shift::Sar => self.bitfield(true, w, d, d, n, size - 1),
        }
    }

    pub fn movsxd(&mut self, dst: R, src: R) {
        self.bitfield(true, true, hw(dst), hw(src), 0, 31);
    }

    pub fn setcc(&mut self, cc: Cc, dst: R) {
        let c = self.cond(cc) as u32 ^ 1;
        self.put(0x1A9F_07E0 | c << 12 | hw(dst));
    }

    pub fn cmov(&mut self, cc: Cc, dst: R, src: R) {
        let c = self.cond(cc) as u32;
        let d = hw(dst);
        self.put(0x9A80_0000 | d << 16 | c << 12 | hw(src) << 5 | d);
    }

    pub fn jcc(&mut self, cc: Cc, l: Label) {
        let c = self.cond(cc) as u32;
        let at = self.here();
        self.put(0x5400_0000 | c);
        self.fixups.push((at, l, Fix::Cond));
    }

    pub fn jmp(&mut self, l: Label) {
        let at = self.here();
        self.put(0x1400_0000);
        self.fixups.push((at, l, Fix::Jump));
    }

    pub fn call_abs(&mut self, target: u64) -> usize {
        self.put(0x5800_0040 | T0);
        self.put(0x1400_0003);
        let at = self.here();
        self.bytes.extend_from_slice(&target.to_le_bytes());
        self.put(0xD63F_0000 | T0 << 5);
        self.mov(hw(RAX), RET0);
        self.mov(hw(RDX), RET1);
        at
    }

    pub fn enter(&mut self, frame: i32) {
        self.put(0xA9BF_0000 | LR << 10 | SP << 5 | FP);
        self.mov(FP, SP);
        #[cfg(windows)]
        self.probe_stack(frame - SAVED_END);
        self.add_imm(SP, FP, i64::from(SAVED_END - frame));
        for (i, r) in (1..).zip(SAVED) {
            self.store(RBP, -8 * i, r);
        }
    }

    #[cfg(windows)]
    pub fn probe_stack(&mut self, frame: i32) {
        const PAGE: i32 = 4096;
        if frame < PAGE {
            return;
        }
        self.mov_rr(R11, RSP);
        self.mov_ri(RAX, u64::from(u32::try_from(frame).unwrap_or(0)));
        let again = self.label();
        self.bind(again);
        self.alu_ri(Alu::Sub, true, R11, PAGE);
        self.store(R11, 0, RAX);
        self.alu_ri(Alu::Sub, true, RAX, PAGE);
        self.alu_ri(Alu::Cmp, true, RAX, PAGE);
        self.jcc(Cc::Ae, again);
        let done = self.label();
        self.test_rr(true, RAX, RAX);
        self.jcc(Cc::E, done);
        self.alu_rr(Alu::Sub, true, R11, RAX);
        self.store(R11, 0, RAX);
        self.bind(done);
    }

    pub fn leave(&mut self) {
        for (i, r) in (1..).zip(SAVED) {
            self.load(r, RBP, -8 * i);
        }
        self.mov(SP, FP);
        self.put(0xA8C1_0000 | LR << 10 | SP << 5 | FP);
        self.mov(RET0, hw(RAX));
        self.mov(RET1, hw(RDX));
        self.put(0xD65F_03C0);
    }

    pub fn save(&mut self, regs: &[R]) {
        for pair in regs.chunks(2) {
            match *pair {
                [a, b] => self.put(0xA9BF_0000 | hw(b) << 10 | SP << 5 | hw(a)),
                [a] => self.put(0xF800_0C00 | field(i64::from(-SLOT), 9) << 12 | SP << 5 | hw(a)),
                _ => {}
            }
        }
    }

    pub fn restore(&mut self, regs: &[R]) {
        for pair in regs.chunks(2).rev() {
            match *pair {
                [a, b] => self.put(0xA8C1_0000 | hw(b) << 10 | SP << 5 | hw(a)),
                [a] => self.put(0xF840_0400 | field(i64::from(SLOT), 9) << 12 | SP << 5 | hw(a)),
                _ => {}
            }
        }
    }

    pub fn movq_xr(&mut self, x: X, r: R) {
        self.put(0x9E67_0000 | hw(r) << 5 | fx(x));
    }

    pub fn movq_rx(&mut self, r: R, x: X) {
        self.put(0x9E66_0000 | fx(x) << 5 | hw(r));
    }

    pub fn movsd_xx(&mut self, dst: X, src: X) {
        if dst != src {
            self.put(0x1E60_4000 | fx(src) << 5 | fx(dst));
        }
    }

    pub fn movsd_load(&mut self, x: X, base: R, disp: i32) {
        self.mem(Mem::D, true, fx(x), hw(base), disp);
    }

    pub fn movsd_store(&mut self, base: R, disp: i32, x: X) {
        self.mem(Mem::D, false, fx(x), hw(base), disp);
    }

    pub fn float(&mut self, op: FloatOp, dst: X, src: X) {
        let code = match op {
            FloatOp::Add => 0x1E60_2800,
            FloatOp::Mul => 0x1E60_0800,
            FloatOp::Sub => 0x1E60_3800,
            FloatOp::Div => 0x1E60_1800,
        };
        let d = fx(dst);
        self.put(code | fx(src) << 16 | d << 5 | d);
    }

    pub fn ucomisd(&mut self, a: X, b: X) {
        self.put(0x1E60_2000 | fx(b) << 16 | fx(a) << 5);
        self.flags = Flags::Float;
    }

    pub fn cvtsi2sd(&mut self, x: X, r: R) {
        self.put(0x1E62_0000 | hw(r) << 5 | fx(x));
    }

    pub fn cvttsd2si(&mut self, r: R, x: X) {
        self.put(0x1E78_0000 | fx(x) << 5 | hw(r));
    }

    pub fn cvtsi2sd64(&mut self, x: X, r: R) {
        self.put(0x9E62_0000 | hw(r) << 5 | fx(x));
    }

    pub fn cvttsd2si64(&mut self, r: R, x: X) {
        self.put(0x9E78_0000 | fx(x) << 5 | hw(r));
    }

    pub fn zero_x(&mut self, x: X) {
        self.put(0x9E67_0000 | ZR << 5 | fx(x));
    }
}
