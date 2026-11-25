use alloc::format;
use alloc::string::String;
use alloc::vec;
use alloc::vec::Vec;
use core::fmt::Write;

use j8bytecode::{Decoded, OperandValue, WasmBytecode};
use j8executor::{WasmCtx, WasmJitFunction, WasmMachine, WasmScalar, WasmTier};
use j8lockfree::{AtomicRefCell, OnceBox};
use mlirpass::{HoleKind, Stencil, Stencils};

use crate::asm::{Alu, Asm, Cc, Label, RAX, RCX, RBP, RDI, RSI, R15};
use crate::live::Template;
use crate::runtime::Arena;
use crate::Compiler;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Ib {
    Add,
    Sub,
    Mul,
    And,
    Or,
    Xor,
    Shl,
    ShrS,
    ShrU,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Ic {
    Eq,
    Ne,
    LtS,
    LtU,
    GtS,
    GtU,
    LeS,
    LeU,
    GeS,
    GeU,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Fb {
    Add,
    Sub,
    Mul,
    Div,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Fc {
    Eq,
    Ne,
    Lt,
    Gt,
    Le,
    Ge,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Ld {
    I32,
    I64,
    S8To32,
    U8To32,
    S16To32,
    U16To32,
    S8To64,
    U8To64,
    S16To64,
    U16To64,
    S32To64,
    U32To64,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum St {
    Bin32(Ib),
    Bin64(Ib),
    Cmp32(Ic),
    Cmp64(Ic),
    Eqz32,
    Eqz64,
    FBin(Fb),
    FCmp(Fc),
    Const,
    Select,
    BrIf { nonzero: bool, wide: bool },
    BrEq,
    Load(Ld, bool),
    Store(u8, bool),
    Scalar,
    Step,
    Call,
    Global { set: bool },
    Indirect,
    CallRef,
}

const IBS: [Ib; 9] = [Ib::Add, Ib::Sub, Ib::Mul, Ib::And, Ib::Or, Ib::Xor, Ib::Shl, Ib::ShrS, Ib::ShrU];
const ICS: [Ic; 10] = [Ic::Eq, Ic::Ne, Ic::LtS, Ic::LtU, Ic::GtS, Ic::GtU, Ic::LeS, Ic::LeU, Ic::GeS, Ic::GeU];
const FBS: [Fb; 4] = [Fb::Add, Fb::Sub, Fb::Mul, Fb::Div];
const FCS: [Fc; 6] = [Fc::Eq, Fc::Ne, Fc::Lt, Fc::Gt, Fc::Le, Fc::Ge];
const LDS: [Ld; 12] = [Ld::I32, Ld::I64, Ld::S8To32, Ld::U8To32, Ld::S16To32, Ld::U16To32, Ld::S8To64, Ld::U8To64, Ld::S16To64, Ld::U16To64, Ld::S32To64, Ld::U32To64];
const STORE_WIDTHS: [u8; 4] = [1, 2, 4, 8];
const STEP: &str = "j8_wasm_step";
const SCALAR: &str = "j8_wasm_scalar";
const CALL: &str = "j8_wasm_call";
const GLOBAL: &str = "j8_wasm_global";
const INDIRECT: &str = "j8_wasm_indirect";
const CALL_REF: &str = "j8_wasm_call_ref";

impl Ib {
    fn llvm(self) -> &'static str {
        match self {
            Ib::Add => "add",
            Ib::Sub => "sub",
            Ib::Mul => "mul",
            Ib::And => "and",
            Ib::Or => "or",
            Ib::Xor => "xor",
            Ib::Shl => "shl",
            Ib::ShrS => "ashr",
            Ib::ShrU => "lshr",
        }
    }

    fn shift(self) -> bool {
        matches!(self, Ib::Shl | Ib::ShrS | Ib::ShrU)
    }
}

impl Ic {
    fn llvm(self) -> &'static str {
        match self {
            Ic::Eq => "eq",
            Ic::Ne => "ne",
            Ic::LtS => "slt",
            Ic::LtU => "ult",
            Ic::GtS => "sgt",
            Ic::GtU => "ugt",
            Ic::LeS => "sle",
            Ic::LeU => "ule",
            Ic::GeS => "sge",
            Ic::GeU => "uge",
        }
    }
}

impl Ld {
    fn shape(self) -> (&'static str, u64, Option<bool>, &'static str) {
        match self {
            Ld::I32 => ("i32", 4, Some(false), "i32"),
            Ld::I64 => ("i64", 8, None, "i64"),
            Ld::S8To32 => ("i8", 1, Some(true), "i32"),
            Ld::U8To32 => ("i8", 1, Some(false), "i32"),
            Ld::S16To32 => ("i16", 2, Some(true), "i32"),
            Ld::U16To32 => ("i16", 2, Some(false), "i32"),
            Ld::S8To64 => ("i8", 1, Some(true), "i64"),
            Ld::U8To64 => ("i8", 1, Some(false), "i64"),
            Ld::S16To64 => ("i16", 2, Some(true), "i64"),
            Ld::U16To64 => ("i16", 2, Some(false), "i64"),
            Ld::S32To64 => ("i32", 4, Some(true), "i64"),
            Ld::U32To64 => ("i32", 4, Some(false), "i64"),
        }
    }
}

impl St {
    fn all() -> Vec<St> {
        let mut out = Vec::new();
        for ib in IBS {
            out.extend([St::Bin32(ib), St::Bin64(ib)]);
        }
        for ic in ICS {
            out.extend([St::Cmp32(ic), St::Cmp64(ic)]);
        }
        out.extend([St::Eqz32, St::Eqz64]);
        out.extend(FBS.map(St::FBin));
        out.extend(FCS.map(St::FCmp));
        out.extend([St::Const, St::Select, St::BrEq, St::Scalar, St::Step, St::Call, St::Global { set: false }, St::Global { set: true }, St::Indirect, St::CallRef]);
        for nonzero in [false, true] {
            for wide in [false, true] {
                out.push(St::BrIf { nonzero, wide });
            }
        }
        for wide in [false, true] {
            out.extend(LDS.map(|l| St::Load(l, wide)));
            out.extend(STORE_WIDTHS.map(|w| St::Store(w, wide)));
        }
        out
    }

    fn name(self) -> String {
        format!("wasm_{self:?}").replace(['(', ')', ',', ' ', '{', '}', ':'], "_")
    }

    fn template(self) -> String {
        let mut t = Tpl::default();
        t.build(self);
        format!("  llvm.func @{}(%ctx: i64, %fp: i64) attributes {{stencil}} {{\n{}    llvm.return\n  }}\n", self.name(), t.out)
    }
}

#[derive(Default)]
struct Tpl {
    out: String,
    n: u32,
    blocks: u32,
}

impl Tpl {
    fn line(&mut self, text: &str) -> String {
        self.n += 1;
        let v = format!("%v{}", self.n);
        let _ = writeln!(self.out, "    {v} = {text}");
        v
    }

    fn stmt(&mut self, text: &str) {
        let _ = writeln!(self.out, "    {text}");
    }

    fn c64(&mut self, x: u64) -> String {
        self.line(&format!("llvm.mlir.constant({} : i64) : i64", x.cast_signed()))
    }

    fn c32(&mut self, x: i32) -> String {
        self.line(&format!("llvm.mlir.constant({x} : i32) : i32"))
    }

    fn hole(&mut self, k: u8) -> String {
        self.line(&format!("llvm.call @hole.{k}() : () -> i64"))
    }

    fn slot_addr(&mut self, k: &str) -> String {
        let o = self.line(&format!("llvm.call @slot.{k}() : () -> i64"));
        self.line(&format!("llvm.add %fp, {o} : i64"))
    }

    fn ptr(&mut self, addr: &str) -> String {
        self.line(&format!("llvm.inttoptr {addr} : i64 to !llvm.ptr"))
    }

    fn read(&mut self, k: u8, ty: &str) -> String {
        let a = self.slot_addr(&format!("{k}"));
        let p = self.ptr(&a);
        self.line(&format!("llvm.load {p} : !llvm.ptr -> {ty}"))
    }

    fn write(&mut self, v: &str) {
        let a = self.slot_addr("d");
        let p = self.ptr(&a);
        self.stmt(&format!("llvm.store {v}, {p} : i64, !llvm.ptr"));
    }

    fn write_f(&mut self, v: &str) {
        let a = self.slot_addr("d");
        let p = self.ptr(&a);
        self.stmt(&format!("llvm.store {v}, {p} : f64, !llvm.ptr"));
    }

    fn widen(&mut self, v: &str, from: &str, signed: bool) -> String {
        let op = if signed { "sext" } else { "zext" };
        self.line(&format!("llvm.{op} {v} : {from} to i64"))
    }

    fn ctx_addr(&mut self, off: i32) -> String {
        let o = self.c64(u64::from(off.unsigned_abs()));
        let a = self.line(&format!("llvm.add %ctx, {o} : i64"));
        self.ptr(&a)
    }

    fn ctx(&mut self, off: i32) -> String {
        let p = self.ctx_addr(off);
        self.line(&format!("llvm.load {p} : !llvm.ptr -> i64"))
    }

    fn check(&mut self, status: &str) {
        let z = self.c64(0);
        let bad = self.icmp("ne", status, &z, "i64");
        self.jump_if(&bad, 0);
    }

    fn label(&mut self) -> String {
        self.blocks += 1;
        format!("^b{}", self.blocks)
    }

    fn jump_if(&mut self, c: &str, jump: u8) {
        let (take, stay) = (self.label(), self.label());
        self.stmt(&format!("llvm.cond_br {c}, {take}, {stay}"));
        let _ = writeln!(self.out, "  {take}:\n    llvm.call @jump.{jump}() : () -> ()\n    llvm.unreachable\n  {stay}:");
    }

    fn icmp(&mut self, p: &str, a: &str, b: &str, ty: &str) -> String {
        self.line(&format!("llvm.icmp \"{p}\" {a}, {b} : {ty}"))
    }

    fn bin(&mut self, op: &str, a: &str, b: &str, ty: &str) -> String {
        self.line(&format!("llvm.{op} {a}, {b} : {ty}"))
    }

    fn address(&mut self, size: u64, wide: bool) -> String {
        let addr = if wide {
            self.read(0, "i64")
        } else {
            let a = self.read(0, "i32");
            self.widen(&a, "i32", false)
        };
        let off = self.hole(0);
        let ea = self.bin("add", &addr, &off, "i64");
        if wide {
            let over = self.icmp("ult", &ea, &addr, "i64");
            self.jump_if(&over, 0);
        }
        let n = self.c64(size);
        let end = self.bin("add", &ea, &n, "i64");
        if wide {
            let over = self.icmp("ult", &end, &ea, "i64");
            self.jump_if(&over, 0);
        }
        let len = self.ctx(WasmCtx::MEM_LEN);
        let out = self.icmp("ugt", &end, &len, "i64");
        self.jump_if(&out, 0);
        let base = self.ctx(WasmCtx::MEM_BASE);
        let a = self.bin("add", &base, &ea, "i64");
        self.ptr(&a)
    }

    #[expect(clippy::too_many_lines, reason = "one arm per stencil kind")]
    fn build(&mut self, st: St) {
        match st {
            St::Bin32(op) => {
                let (a, b) = (self.read(0, "i32"), self.read(1, "i32"));
                let b = if op.shift() {
                    let m = self.c32(31);
                    self.bin("and", &b, &m, "i32")
                } else {
                    b
                };
                let r = self.bin(op.llvm(), &a, &b, "i32");
                let w = self.widen(&r, "i32", false);
                self.write(&w);
            }
            St::Bin64(op) => {
                let (a, b) = (self.read(0, "i64"), self.read(1, "i64"));
                let b = if op.shift() {
                    let m = self.c64(63);
                    self.bin("and", &b, &m, "i64")
                } else {
                    b
                };
                let r = self.bin(op.llvm(), &a, &b, "i64");
                self.write(&r);
            }
            St::Cmp32(c) => {
                let (a, b) = (self.read(0, "i32"), self.read(1, "i32"));
                let r = self.icmp(c.llvm(), &a, &b, "i32");
                let w = self.widen(&r, "i1", false);
                self.write(&w);
            }
            St::Cmp64(c) => {
                let (a, b) = (self.read(0, "i64"), self.read(1, "i64"));
                let r = self.icmp(c.llvm(), &a, &b, "i64");
                let w = self.widen(&r, "i1", false);
                self.write(&w);
            }
            St::Eqz32 => {
                let a = self.read(0, "i32");
                let z = self.c32(0);
                let r = self.icmp("eq", &a, &z, "i32");
                let w = self.widen(&r, "i1", false);
                self.write(&w);
            }
            St::Eqz64 => {
                let a = self.read(0, "i64");
                let z = self.c64(0);
                let r = self.icmp("eq", &a, &z, "i64");
                let w = self.widen(&r, "i1", false);
                self.write(&w);
            }
            St::FBin(op) => {
                let (a, b) = (self.read(0, "f64"), self.read(1, "f64"));
                let name = match op {
                    Fb::Add => "fadd",
                    Fb::Sub => "fsub",
                    Fb::Mul => "fmul",
                    Fb::Div => "fdiv",
                };
                let r = self.bin(name, &a, &b, "f64");
                self.write_f(&r);
            }
            St::FCmp(c) => {
                let (a, b) = (self.read(0, "f64"), self.read(1, "f64"));
                let p = match c {
                    Fc::Eq => "oeq",
                    Fc::Ne => "une",
                    Fc::Lt => "olt",
                    Fc::Gt => "ogt",
                    Fc::Le => "ole",
                    Fc::Ge => "oge",
                };
                let r = self.line(&format!("llvm.fcmp \"{p}\" {a}, {b} : f64"));
                let w = self.widen(&r, "i1", false);
                self.write(&w);
            }
            St::Const => {
                let k = self.hole(0);
                self.write(&k);
            }
            St::Select => {
                let (a, b, c) = (self.read(0, "i64"), self.read(1, "i64"), self.read(2, "i32"));
                let z = self.c32(0);
                let t = self.icmp("ne", &c, &z, "i32");
                let r = self.line(&format!("llvm.select {t}, {a}, {b} : i1, i64"));
                self.write(&r);
            }
            St::BrIf { nonzero, wide } => {
                let (x, z) = if wide { (self.read(0, "i64"), self.c64(0)) } else { (self.read(0, "i32"), self.c32(0)) };
                let c = self.icmp(if nonzero { "ne" } else { "eq" }, &x, &z, if wide { "i64" } else { "i32" });
                self.jump_if(&c, 0);
            }
            St::BrEq => {
                let x = self.read(0, "i32");
                let k = self.hole(0);
                let k = self.line(&format!("llvm.trunc {k} : i64 to i32"));
                let c = self.icmp("eq", &x, &k, "i32");
                self.jump_if(&c, 0);
            }
            St::Load(ld, wide) => {
                let (ty, size, ext, to) = ld.shape();
                let p = self.address(size, wide);
                let v = self.line(&format!("llvm.load {p} : !llvm.ptr -> {ty}"));
                let r = match ext {
                    None => v,
                    Some(signed) if to == "i32" && ty != "i32" => {
                        let op = if signed { "sext" } else { "zext" };
                        let x = self.line(&format!("llvm.{op} {v} : {ty} to i32"));
                        self.widen(&x, "i32", false)
                    }
                    Some(signed) => self.widen(&v, ty, signed),
                };
                self.write(&r);
            }
            St::Store(width, wide) => {
                let p = self.address(u64::from(width), wide);
                let v = self.read(1, "i64");
                let ty = format!("i{}", u32::from(width) * u8::BITS);
                let x = if width == 8 { v } else { self.line(&format!("llvm.trunc {v} : i64 to {ty}")) };
                self.stmt(&format!("llvm.store {x}, {p} : {ty}, !llvm.ptr"));
            }
            St::Scalar => {
                let op = self.hole(0);
                let (a, b) = (self.read(0, "i64"), self.read(1, "i64"));
                let r = self.line(&format!("llvm.call @{SCALAR}(%ctx, {op}, {a}, {b}) : (i64, i64, i64, i64) -> i64"));
                let s = self.ctx(WasmCtx::STATUS);
                let z = self.c64(0);
                let bad = self.icmp("ne", &s, &z, "i64");
                self.jump_if(&bad, 0);
                self.write(&r);
            }
            St::Step => {
                let pc = self.hole(0);
                let disp = self.hole(1);
                let code = self.hole(2);
                let regs = self.bin("add", "%fp", &disp, "i64");
                let r = self.line(&format!("llvm.call @{STEP}(%ctx, {pc}, {regs}, {code}) : (i64, i64, i64, i64) -> i64"));
                self.check(&r);
            }
            St::Call => {
                let index = self.hole(0);
                let disp = self.hole(1);
                let regs = self.bin("add", "%fp", &disp, "i64");
                let r = self.line(&format!("llvm.call @{CALL}(%ctx, {index}, {regs}) : (i64, i64, i64) -> i64"));
                self.check(&r);
            }
            St::Global { set } => {
                let index = self.hole(0);
                let (v, flag) = if set { (self.read(0, "i64"), self.c64(1)) } else { (self.c64(0), self.c64(0)) };
                let r = self.line(&format!("llvm.call @{GLOBAL}(%ctx, {index}, {v}, {flag}) : (i64, i64, i64, i64) -> i64"));
                if !set {
                    self.write(&r);
                }
            }
            St::Indirect | St::CallRef => {
                let packed = self.hole(0);
                let disp = self.hole(1);
                let regs = self.bin("add", "%fp", &disp, "i64");
                let x = self.read(0, "i64");
                let callee = if st == St::Indirect { INDIRECT } else { CALL_REF };
                let r = self.line(&format!("llvm.call @{callee}(%ctx, {packed}, {x}, {regs}) : (i64, i64, i64, i64) -> i64"));
                let z = self.c64(0);
                let bad = self.icmp("ne", &r, &z, "i64");
                self.jump_if(&bad, 0);
            }
        }
    }
}

#[derive(Clone, Copy, Debug)]
enum Sym {
    Step,
    Scalar,
    Call,
    Global,
    Indirect,
    CallRef,
}

#[derive(Clone, Copy, Debug)]
enum Fill {
    Operand(u8),
    Dest,
    Const(u8),
    Jump,
    Sym(Sym),
}

struct Piece {
    code: Vec<u8>,
    holes: Vec<(usize, HoleKind, Fill)>,
}

impl TryFrom<&Stencil> for Piece {
    type Error = String;

    fn try_from(s: &Stencil) -> Result<Piece, String> {
        let mut holes = Vec::with_capacity(s.holes.len());
        for h in &s.holes {
            let num = |p: &str| h.name.strip_prefix(p).and_then(|k| k.parse::<u8>().ok());
            let fill = if h.name == "slot.d" {
                Fill::Dest
            } else if let Some(k) = num("slot.") {
                Fill::Operand(k)
            } else if let Some(k) = num("hole.") {
                Fill::Const(k)
            } else if num("jump.").is_some() {
                Fill::Jump
            } else if h.name == STEP {
                Fill::Sym(Sym::Step)
            } else if h.name == SCALAR {
                Fill::Sym(Sym::Scalar)
            } else if h.name == CALL {
                Fill::Sym(Sym::Call)
            } else if h.name == GLOBAL {
                Fill::Sym(Sym::Global)
            } else if h.name == INDIRECT {
                Fill::Sym(Sym::Indirect)
            } else if h.name == CALL_REF {
                Fill::Sym(Sym::CallRef)
            } else {
                return Err(format!("a wasm stencil refers to unknown @{}", h.name));
            };
            holes.push((h.at, h.kind, fill));
        }
        Ok(Piece { code: s.code.clone(), holes })
    }
}

struct Table {
    pieces: Vec<(St, Piece)>,
}

impl Table {
    fn compile() -> Result<Table, String> {
        let all = St::all();
        let mut text = String::from("module {\n");
        for st in &all {
            text.push_str(&st.template());
        }
        text.push_str("}\n");
        #[cfg(target_arch = "aarch64")]
        let emit = crate::aarch64::AArch64Emit;
        #[cfg(not(target_arch = "aarch64"))]
        let emit = crate::X86_64Emit;
        let stencils: Stencils = Compiler::default().stencils(&text, &emit).map_err(|e| format!("{e:?}"))?;
        let mut pieces = Vec::with_capacity(all.len());
        for st in all {
            let s = stencils.get(&st.name()).ok_or_else(|| format!("no stencil {}", st.name()))?;
            pieces.push((st, Piece::try_from(s)?));
        }
        Ok(Table { pieces })
    }

    fn get(&self, st: St) -> Option<&Piece> {
        self.pieces.iter().find(|(s, _)| *s == st).map(|(_, p)| p)
    }
}

struct Stitch<'t, 'f> {
    table: &'t Table,
    f: &'f WasmJitFunction<'f>,
    a: Asm,
    base: i32,
    labels: Vec<Label>,
    jumps: Vec<(usize, Label, HoleKind)>,
    relocs: Vec<(usize, Sym)>,
    oob: Label,
    exit: Label,
    epilogue: Label,
    osr: Label,
}

struct Stitched {
    bytes: Vec<u8>,
    relocs: Vec<(usize, Sym)>,
    osr: usize,
}

struct Use<'p> {
    piece: &'p Piece,
    operands: Vec<i32>,
    dest: Option<i32>,
    consts: Vec<u64>,
    jump: Option<Label>,
}

impl<'t, 'f> Stitch<'t, 'f> {
    const FIRST_SLOT: i32 = Template::SPILL_TOP - 8 * Template::SPILL_WORDS - 8;

    fn reg(&self, r: OperandValue) -> i32 {
        self.base + 8 * i32::try_from(r).unwrap_or(0)
    }

    fn io(&self) -> i32 {
        self.base - 8
    }

    fn index(&self, d: &Decoded<WasmBytecode>, rel: OperandValue) -> Option<usize> {
        let byte = usize::try_from(OperandValue::try_from(d.offset).ok()? + rel).ok()?;
        Some(*self.f.at.get(byte)? as usize)
    }

    fn target(&self, d: &Decoded<WasmBytecode>, rel: OperandValue) -> Option<Label> {
        let i = self.index(d, rel)?;
        self.labels.get(i).copied()
    }

    fn headers(&self) -> Vec<usize> {
        use WasmBytecode as B;
        let mut out: Vec<usize> = self
            .f
            .ops
            .iter()
            .enumerate()
            .filter_map(|(pc, d)| {
                let rel = match d.bytecode {
                    B::Jump => d.operands[0],
                    B::JumpIfZero | B::JumpIfNonZero => d.operands[1],
                    _ => return None,
                };
                self.index(d, rel).filter(|&t| t <= pc)
            })
            .collect();
        out.sort_unstable();
        out.dedup();
        out
    }

    fn prologue(&mut self, frame: i32, from: u32) -> Option<()> {
        self.a.enter(frame);
        self.a.mov_rr(R15, RDI);
        self.a.store(RBP, self.io(), RSI);
        for i in 0..from {
            self.a.load(RAX, RSI, 8 * i32::try_from(i).ok()?);
            self.a.store(RBP, self.reg(OperandValue::from(i)), RAX);
        }
        self.a.mov_ri(RAX, 0);
        for r in from..self.f.registers {
            self.a.store(RBP, self.reg(OperandValue::from(r)), RAX);
        }
        Some(())
    }

    fn put(&mut self, u: &Use<'_>) {
        let start = self.a.here();
        self.a.bytes.extend_from_slice(&u.piece.code);
        for &(at, kind, fill) in &u.piece.holes {
            let at = start + at;
            match (kind, fill) {
                (HoleKind::Disp32, Fill::Operand(k)) => Stencil::disp32(&mut self.a.bytes, at, u.operands.get(k as usize).copied().unwrap_or(0)),
                (HoleKind::Disp32, Fill::Dest) => Stencil::disp32(&mut self.a.bytes, at, u.dest.unwrap_or(0)),
                (HoleKind::Abs64, Fill::Const(k)) => Stencil::abs64(&mut self.a.bytes, at, u.consts.get(k as usize).copied().unwrap_or(0)),
                (HoleKind::Abs64, Fill::Sym(s)) => self.relocs.push((at, s)),
                (HoleKind::Rel32 | HoleKind::Branch26, Fill::Jump) => {
                    if let Some(l) = u.jump {
                        self.jumps.push((at, l, kind));
                    }
                }
                _ => {}
            }
        }
    }

    fn emit(&mut self, st: St, operands: &[OperandValue], dest: Option<OperandValue>, consts: Vec<u64>, jump: Option<Label>) -> Option<()> {
        let piece = self.table.get(st)?;
        let operands = operands.iter().map(|&r| self.reg(r)).collect();
        let dest = dest.map(|r| self.reg(r));
        self.put(&Use { piece, operands, dest, consts, jump });
        Some(())
    }

    fn mov(&mut self, dst: OperandValue, src: OperandValue) {
        if dst != src {
            self.a.load(RAX, RBP, self.reg(src));
            self.a.store(RBP, self.reg(dst), RAX);
        }
    }

    fn ret(&mut self, first: OperandValue, n: u32) {
        self.a.load(RCX, RBP, self.io());
        for i in 0..n {
            self.a.load(RAX, RBP, self.reg(first + OperandValue::from(i)));
            self.a.store(RCX, 8 * i32::try_from(i).unwrap_or(0), RAX);
        }
        self.a.mov_ri(RAX, WasmCtx::RETURNED);
        self.a.jmp(self.epilogue);
    }

    fn step(&mut self, pc: usize) -> Option<()> {
        let exit = self.exit;
        let disp = i64::from(self.base).cast_unsigned();
        self.emit(St::Step, &[], None, vec![u64::try_from(pc).ok()?, disp, self.f.code], Some(exit))
    }

    fn direct(&mut self, cell: u64, disp: u64, index: u64) -> Option<()> {
        let (slow, done, exit) = (self.a.label(), self.a.label(), self.exit);
        self.a.mov_ri(RAX, cell);
        self.a.load(RAX, RAX, 0);
        self.a.test_rr(true, RAX, RAX);
        self.a.jcc(Cc::E, slow);
        self.a.cmp_mi(true, R15, WasmCtx::DEPTH, i32::try_from(WasmCtx::NEST).ok()?);
        self.a.jcc(Cc::Ae, slow);
        self.depth(Alu::Add);
        self.a.mov_rr(RDI, R15);
        self.a.lea(RSI, RBP, i32::try_from(disp.cast_signed()).ok()?);
        self.a.call_r(RAX);
        self.depth(Alu::Sub);
        self.a.test_rr(true, RAX, RAX);
        self.a.jcc(Cc::Ne, exit);
        self.a.jmp(done);
        self.a.bind(slow);
        self.emit(St::Call, &[], None, vec![index, disp], Some(exit))?;
        self.a.bind(done);
        Some(())
    }

    fn depth(&mut self, op: Alu) {
        self.a.load(RCX, R15, WasmCtx::DEPTH);
        self.a.alu_ri(op, true, RCX, 1);
        self.a.store(R15, WasmCtx::DEPTH, RCX);
    }

    fn memory(&self, d: &Decoded<WasmBytecode>, at: usize) -> Option<bool> {
        if d.operands[at] != 0 {
            return None;
        }
        self.f.memory64
    }

    #[expect(clippy::too_many_lines, reason = "one arm per bytecode with a stencil")]
    fn op(&mut self, pc: usize, d: &Decoded<WasmBytecode>) -> Option<()> {
        use WasmBytecode as B;
        let o = d.operands;
        let oob = self.oob;
        let bin32 = |op| Some((St::Bin32(op), 2));
        let bin64 = |op| Some((St::Bin64(op), 2));
        let simple = match d.bytecode {
            B::I32Add => bin32(Ib::Add),
            B::I32Sub => bin32(Ib::Sub),
            B::I32Mul => bin32(Ib::Mul),
            B::I32And => bin32(Ib::And),
            B::I32Ior => bin32(Ib::Or),
            B::I32Xor => bin32(Ib::Xor),
            B::I32Shl => bin32(Ib::Shl),
            B::I32ShrS => bin32(Ib::ShrS),
            B::I32ShrU => bin32(Ib::ShrU),
            B::I64Add => bin64(Ib::Add),
            B::I64Sub => bin64(Ib::Sub),
            B::I64Mul => bin64(Ib::Mul),
            B::I64And => bin64(Ib::And),
            B::I64Ior => bin64(Ib::Or),
            B::I64Xor => bin64(Ib::Xor),
            B::I64Shl => bin64(Ib::Shl),
            B::I64ShrS => bin64(Ib::ShrS),
            B::I64ShrU => bin64(Ib::ShrU),
            B::I32Eq => Some((St::Cmp32(Ic::Eq), 2)),
            B::I32Ne => Some((St::Cmp32(Ic::Ne), 2)),
            B::I32LtS => Some((St::Cmp32(Ic::LtS), 2)),
            B::I32LtU => Some((St::Cmp32(Ic::LtU), 2)),
            B::I32GtS => Some((St::Cmp32(Ic::GtS), 2)),
            B::I32GtU => Some((St::Cmp32(Ic::GtU), 2)),
            B::I32LeS => Some((St::Cmp32(Ic::LeS), 2)),
            B::I32LeU => Some((St::Cmp32(Ic::LeU), 2)),
            B::I32GeS => Some((St::Cmp32(Ic::GeS), 2)),
            B::I32GeU => Some((St::Cmp32(Ic::GeU), 2)),
            B::I64Eq | B::RefEq => Some((St::Cmp64(Ic::Eq), 2)),
            B::I64Ne => Some((St::Cmp64(Ic::Ne), 2)),
            B::I64LtS => Some((St::Cmp64(Ic::LtS), 2)),
            B::I64LtU => Some((St::Cmp64(Ic::LtU), 2)),
            B::I64GtS => Some((St::Cmp64(Ic::GtS), 2)),
            B::I64GtU => Some((St::Cmp64(Ic::GtU), 2)),
            B::I64LeS => Some((St::Cmp64(Ic::LeS), 2)),
            B::I64LeU => Some((St::Cmp64(Ic::LeU), 2)),
            B::I64GeS => Some((St::Cmp64(Ic::GeS), 2)),
            B::I64GeU => Some((St::Cmp64(Ic::GeU), 2)),
            B::I32Eqz => Some((St::Eqz32, 1)),
            B::I64Eqz | B::RefIsNull => Some((St::Eqz64, 1)),
            B::F64Add => Some((St::FBin(Fb::Add), 2)),
            B::F64Sub => Some((St::FBin(Fb::Sub), 2)),
            B::F64Mul => Some((St::FBin(Fb::Mul), 2)),
            B::F64Div => Some((St::FBin(Fb::Div), 2)),
            B::F64Eq => Some((St::FCmp(Fc::Eq), 2)),
            B::F64Ne => Some((St::FCmp(Fc::Ne), 2)),
            B::F64Lt => Some((St::FCmp(Fc::Lt), 2)),
            B::F64Gt => Some((St::FCmp(Fc::Gt), 2)),
            B::F64Le => Some((St::FCmp(Fc::Le), 2)),
            B::F64Ge => Some((St::FCmp(Fc::Ge), 2)),
            _ => None,
        };
        if let Some((st, n)) = simple {
            return self.emit(st, &o[1..=n], Some(o[0]), Vec::new(), None);
        }
        let load = |ld| Some(St::Load(ld, false));
        let loaded = match d.bytecode {
            B::I32LoadMem | B::F32LoadMem => load(Ld::I32),
            B::I64LoadMem | B::F64LoadMem => load(Ld::I64),
            B::I32LoadMem8S => load(Ld::S8To32),
            B::I32LoadMem8U => load(Ld::U8To32),
            B::I32LoadMem16S => load(Ld::S16To32),
            B::I32LoadMem16U => load(Ld::U16To32),
            B::I64LoadMem8S => load(Ld::S8To64),
            B::I64LoadMem8U => load(Ld::U8To64),
            B::I64LoadMem16S => load(Ld::S16To64),
            B::I64LoadMem16U => load(Ld::U16To64),
            B::I64LoadMem32S => load(Ld::S32To64),
            B::I64LoadMem32U => load(Ld::U32To64),
            _ => None,
        };
        if let Some(St::Load(ld, _)) = loaded {
            let Some(wide) = self.memory(d, 2) else { return self.step(pc) };
            let off = u64::try_from(o[3]).ok()?;
            return self.emit(St::Load(ld, wide), &[o[1]], Some(o[0]), vec![off], Some(oob));
        }
        let width = match d.bytecode {
            B::I32StoreMem8 | B::I64StoreMem8 => Some(1),
            B::I32StoreMem16 | B::I64StoreMem16 => Some(2),
            B::I32StoreMem | B::F32StoreMem | B::I64StoreMem32 => Some(4),
            B::I64StoreMem | B::F64StoreMem => Some(8),
            _ => None,
        };
        if let Some(w) = width {
            let Some(wide) = self.memory(d, 2) else { return self.step(pc) };
            let off = u64::try_from(o[3]).ok()?;
            return self.emit(St::Store(w, wide), &[o[0], o[1]], None, vec![off], Some(oob));
        }
        match d.bytecode {
            B::Mov => self.mov(o[0], o[1]),
            B::I32Const | B::F32Const => {
                let v = u64::from(u32::try_from(o[1] & OperandValue::from(u32::MAX)).ok()?);
                self.emit(St::Const, &[], Some(o[0]), vec![v], None)?;
            }
            B::I64Const => self.emit(St::Const, &[], Some(o[0]), vec![o[1].cast_unsigned()], None)?,
            B::I64ConstPool | B::F64Const => {
                let v = *self.f.bits.get(usize::try_from(o[1]).ok()?)?;
                self.emit(St::Const, &[], Some(o[0]), vec![v], None)?;
            }
            B::RefNull => self.emit(St::Const, &[], Some(o[0]), vec![0], None)?,
            B::Select => self.emit(St::Select, &[o[1], o[2], o[3]], Some(o[0]), Vec::new(), None)?,
            B::Jump => {
                let l = self.target(d, o[0])?;
                self.a.jmp(l);
            }
            B::JumpIfZero | B::JumpIfNonZero => {
                let l = self.target(d, o[1])?;
                self.emit(St::BrIf { nonzero: d.bytecode == B::JumpIfNonZero, wide: false }, &[o[0]], None, Vec::new(), Some(l))?;
            }
            B::BrOnNull | B::BrOnNonNull => {
                let l = self.target(d, o[1])?;
                self.emit(St::BrIf { nonzero: d.bytecode == B::BrOnNonNull, wide: true }, &[o[0]], None, Vec::new(), Some(l))?;
            }
            B::BrTable => {
                let table = self.f.tables.get(usize::try_from(o[1]).ok()?)?;
                let (last, entries) = table.split_last()?;
                for (k, &byte) in entries.iter().enumerate() {
                    let l = *self.labels.get(*self.f.at.get(byte as usize)? as usize)?;
                    self.emit(St::BrEq, &[o[0]], None, vec![u64::try_from(k).ok()?], Some(l))?;
                }
                let l = *self.labels.get(*self.f.at.get(*last as usize)? as usize)?;
                self.a.jmp(l);
            }
            B::Return => self.ret(o[0], u32::try_from(o[1]).ok()?),
            B::ReturnCall | B::ReturnCallIndirect | B::ReturnCallRef => {
                self.step(pc)?;
                self.ret(0, self.f.results);
            }
            B::CallFunction => {
                let exit = self.exit;
                let disp = i64::from(self.reg(o[1])).cast_unsigned();
                let index = u64::try_from(o[0]).ok()?;
                match index.checked_sub(u64::from(self.f.imports)) {
                    Some(local) => self.direct(self.f.entries + local * u64::from(u64::BITS / u8::BITS), disp, index)?,
                    None => self.emit(St::Call, &[], None, vec![index, disp], Some(exit))?,
                }
            }
            B::GlobalGet => self.emit(St::Global { set: false }, &[], Some(o[0]), vec![o[1].cast_unsigned()], None)?,
            B::GlobalSet => self.emit(St::Global { set: true }, &[o[1]], None, vec![o[0].cast_unsigned()], None)?,
            B::CallIndirect => {
                let exit = self.exit;
                let packed = (o[1].cast_unsigned() << u32::BITS) | o[0].cast_unsigned();
                let disp = i64::from(self.reg(o[3])).cast_unsigned();
                self.emit(St::Indirect, &[o[2]], None, vec![packed, disp], Some(exit))?;
            }
            B::CallRef => {
                let exit = self.exit;
                let disp = i64::from(self.reg(o[2])).cast_unsigned();
                self.emit(St::CallRef, &[o[1]], None, vec![0, disp], Some(exit))?;
            }
            B::BrOnCast | B::BrOnCastFail => return None,
            op if (op as u16) >> 8 != 0 => return None,
            op if WasmScalar::handles(op) => {
                let n = op.operand_types().len();
                let b = if n > 2 { o[2] } else { o[1] };
                self.emit(St::Scalar, &[o[1], b], Some(o[0]), vec![u64::from(op as u16)], Some(self.exit))?;
            }
            _ => self.step(pc)?,
        }
        Some(())
    }

    fn run(&mut self) -> Option<()> {
        let f = self.f;
        let regs = i32::try_from(f.registers).ok()?;
        let n = Template::SPILL_WORDS + 2 + regs;
        let frame = (n * 8 + 15) / 16 * 16 + 8;
        self.prologue(frame, f.params)?;
        for (pc, d) in f.ops.iter().enumerate() {
            let l = self.labels[pc];
            self.a.bind(l);
            self.op(pc, d)?;
        }
        self.a.bind(self.oob);
        self.a.mov_ri(RAX, WasmCtx::OUT_OF_BOUNDS);
        self.a.store(R15, WasmCtx::TRAP, RAX);
        self.a.mov_ri(RAX, WasmCtx::TRAPPED);
        self.a.store(R15, WasmCtx::STATUS, RAX);
        self.a.bind(self.exit);
        self.a.load(RAX, R15, WasmCtx::STATUS);
        self.a.bind(self.epilogue);
        self.a.leave();
        self.a.bind(self.osr);
        self.prologue(frame, f.registers)?;
        for h in self.headers() {
            self.a.cmp_mi(true, R15, WasmCtx::OSR, i32::try_from(h).ok()?);
            self.a.jcc(Cc::E, self.labels[h]);
        }
        self.a.mov_ri(RAX, WasmCtx::UNSUPPORTED);
        self.a.store(R15, WasmCtx::TRAP, RAX);
        self.a.mov_ri(RAX, WasmCtx::TRAPPED);
        self.a.store(R15, WasmCtx::STATUS, RAX);
        self.a.jmp(self.exit);
        Some(())
    }

    fn stitch(table: &'t Table, f: &'f WasmJitFunction<'f>) -> Option<Stitched> {
        let regs = i32::try_from(f.registers).ok()?;
        let mut a = Asm::default();
        let labels = (0..f.ops.len()).map(|_| a.label()).collect();
        let (oob, exit, epilogue, osr) = (a.label(), a.label(), a.label(), a.label());
        let base = Self::FIRST_SLOT - 8 * (regs - 1).max(0);
        let mut me = Stitch { table, f, a, base, labels, jumps: Vec::new(), relocs: Vec::new(), oob, exit, epilogue, osr };
        me.run()?;
        let _ = me.a.finish();
        for &(at, l, kind) in &me.jumps {
            let to = me.a.at(l)?;
            if kind == HoleKind::Branch26 {
                Stencil::branch26(&mut me.a.bytes, at, to);
            } else {
                Stencil::rel32(&mut me.a.bytes, at, to);
            }
        }
        let osr = me.a.at(me.osr)?;
        Some(Stitched { bytes: me.a.bytes, relocs: me.relocs, osr })
    }
}

struct Code(Arena);

unsafe impl Send for Code {}

unsafe impl Sync for Code {}

pub struct WasmJit {
    threshold: u32,
    table: OnceBox<Option<Table>>,
    code: AtomicRefCell<Code>,
}

impl WasmJit {
    pub const THRESHOLD: u32 = 16;

    fn table(&self) -> Option<&Table> {
        self.table
            .get_or_init(|| {
                Table::compile()
                    .map_err(|e| {
                        std::eprintln!("j8jit: the wasm stencils did not compile: {e}");
                    })
                    .ok()
            })
            .as_ref()
    }

    pub fn ready(&self) -> bool {
        self.table().is_some()
    }
}

impl Default for WasmJit {
    fn default() -> Self {
        Self::from(Self::THRESHOLD)
    }
}

impl From<u32> for WasmJit {
    fn from(threshold: u32) -> Self {
        Self { threshold, table: OnceBox::default(), code: AtomicRefCell::new(Code(Arena::default())) }
    }
}

impl WasmTier for WasmJit {
    fn threshold(&self) -> u32 {
        self.threshold
    }

    fn compile(&self, f: &WasmJitFunction<'_>) -> Option<WasmMachine> {
        let table = self.table()?;
        let Stitched { bytes, relocs, osr } = Stitch::stitch(table, f)?;
        let sym = |s: Sym| match s {
            Sym::Step => f.step,
            Sym::Scalar => f.scalar,
            Sym::Call => f.call,
            Sym::Global => f.global,
            Sym::Indirect => f.indirect,
            Sym::CallRef => f.call_ref,
        };
        let addr = self.code.try_borrow_mut()?.0.place(&bytes, |buf, _| {
            for &(off, s) in &relocs {
                buf[off..off + 8].copy_from_slice(&sym(s).to_le_bytes());
            }
        })?;
        let call = u64::try_from(addr).ok()?;
        Some(WasmMachine { call, osr: call + u64::try_from(osr).ok()? })
    }
}
