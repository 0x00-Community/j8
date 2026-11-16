use alloc::collections::BTreeSet;
use alloc::string::String;
use alloc::vec::Vec;
use core::fmt::Write;

use j8virtasm::{Constant, Function, Insn, Reg};
use j8framework::{BinaryOp, CompareOp, ValueBits, ValueTag};

use crate::Helper;

/// Symbol name of the machine-code entry function
pub(crate) const ENTRY: &str = "j8_jit_entry";

/// An instruction the first tier does not accept (machine code stops before it and hands it to the interpreter): its
/// index and what it is.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Unsupported {
    pub pc: u32,
    pub insn: Insn,
}

/// Predicates of `llvm.fcmp` (names in the custom syntax)
struct FCmp;

impl FCmp {
    const OEQ: &str = "oeq";
    const OGT: &str = "ogt";
    const OGE: &str = "oge";
    const OLT: &str = "olt";
    const OLE: &str = "ole";
    /// Ordered and not equal (`NaN` gives false)
    const ONE: &str = "one";
    /// Unordered or not equal (`NaN` gives true; the semantics of `!=`)
    const UNE: &str = "une";
    /// Unordered (either is `NaN`)
    const UNO: &str = "uno";

    fn of(op: CompareOp) -> &'static str {
        match op {
            CompareOp::Eq | CompareOp::StrictEq => Self::OEQ,
            CompareOp::Ne | CompareOp::StrictNe => Self::UNE,
            CompareOp::Lt => Self::OLT,
            CompareOp::Gt => Self::OGT,
            CompareOp::Le => Self::OLE,
            CompareOp::Ge => Self::OGE,
        }
    }
}

/// Predicates of `llvm.icmp`
const ICMP_EQ: &str = "eq";
const ICMP_NE: &str = "ne";
/// Unsigned less than
const ICMP_ULT: &str = "ult";

/// One bytecode function ⇒ MLIR text (`llvm` dialect).
///
/// One block `^iN` per instruction; instructions that may bail out to the interpreter also get a `^dN` (returns `N +
/// 1`). The accumulator and each register are one local each (`llvm.alloca`: the NaN-boxed value word): the entry reads them from
/// the frame (the [`Value`](j8framework::Value)s pointed to by `%acc` / `%regs`), and every exit jumps to the
/// write-back block `^x`, which writes them back to the frame and returns; mem2reg later promotes the locals to SSA.
pub(crate) struct Lower<'f> {
    func: &'f Function,
    out: String,
    next: u32,
    /// Instructions that use the exit block
    deopts: BTreeSet<u32>,
    /// Runtime functions used
    helpers: BTreeSet<&'static str>,
    /// Instructions the first tier does not accept, where execution stops and hands off to the interpreter
    exits: Vec<Unsupported>,
}

/// The lowered module and its summary.
pub(crate) struct Lowered {
    pub(crate) text: String,
    pub(crate) exits: Vec<Unsupported>,
    pub(crate) headers: Vec<u32>,
}

/// Name of an SSA value
type V = String;

impl<'f> Lower<'f> {
    /// The whole module: runtime function declarations + the entry function.
    pub(crate) fn module(func: &'f Function) -> Lowered {
        let mut l = Lower { func, out: String::new(), next: 0, deopts: BTreeSet::new(), helpers: BTreeSet::new(), exits: Vec::new() };
        let loops = Self::loops(func);
        for (pc, &insn) in (0u32..).zip(&func.insns) {
            l.label(&alloc::format!("i{pc}"));
            if let Err(u) = l.insn(pc, insn) {
                // An instruction the first tier does not accept: stop before it and hand it to the interpreter
                l.exits.push(u);
                l.deopts.insert(pc);
                l.line(&alloc::format!("llvm.br ^d{pc}"));
            }
        }
        if func.insns.is_empty() {
            l.label("i0");
            let z = l.int(0, 64);
            l.line(&alloc::format!("llvm.br ^x({z} : i64)"));
        }
        for pc in core::mem::take(&mut l.deopts) {
            l.label(&alloc::format!("d{pc}"));
            let c = l.int(u64::from(pc) + 1, 64);
            l.line(&alloc::format!("llvm.br ^x({c} : i64)"));
        }
        // Tail block: write all locals back to the frame at once and return with the exit code
        l.label("x(%code: i64)");
        for (local, base, at) in l.locals() {
            let w = l.def(&alloc::format!("llvm.load {local} : !llvm.ptr -> i64"));
            let p = l.frame_addr(base, at);
            l.line(&alloc::format!("llvm.store {w}, {p} : i64, !llvm.ptr"));
        }
        l.line("llvm.return %code : i64");
        let mut m = String::from("module {\n");
        for h in &l.helpers {
            let _ = writeln!(m, "  llvm.func @{h}(f64, f64) -> f64");
        }
        let _ = writeln!(m, "  llvm.func @{ENTRY}(%regs: !llvm.ptr, %acc: !llvm.ptr, %start: i64) -> i64 {{");
        // Loops that can be entered midway: the loop body has no instruction that stops and hands off to the
        // interpreter (otherwise every iteration would enter and leave once, which doesn't pay off)
        let mut headers: Vec<u32> = loops.iter().filter(|&&(h, back)| !l.exits.iter().any(|u| (h..=back).contains(&u.pc))).map(|&(h, _)| h).collect();
        headers.sort_unstable();
        headers.dedup();
        // Entry dispatch: `%start` 0 enters at the start, `h + 1` enters at loop header `h` (OSR)
        let mut dispatch = Lower { func, out: String::new(), next: l.next, deopts: BTreeSet::new(), helpers: BTreeSet::new(), exits: Vec::new() };
        // Locals (promoted to SSA by mem2reg), read from the frame once
        dispatch.line("%one = llvm.mlir.constant(1 : i64) : i64");
        for (local, base, at) in dispatch.locals() {
            dispatch.line(&alloc::format!("{local} = llvm.alloca %one x i64 : (i64) -> !llvm.ptr"));
            let p = dispatch.frame_addr(base, at);
            let w = dispatch.def(&alloc::format!("llvm.load {p} : !llvm.ptr -> i64"));
            dispatch.line(&alloc::format!("llvm.store {w}, {local} : i64, !llvm.ptr"));
        }
        for &h in &headers {
            let k = dispatch.int(u64::from(h) + 1, 64);
            let hit = dispatch.icmp(ICMP_EQ, "%start", &k, "i64");
            let next = dispatch.fresh();
            let name = next.trim_start_matches('%');
            dispatch.line(&alloc::format!("llvm.cond_br {hit}, ^i{h}, ^e{name}"));
            dispatch.label(&alloc::format!("e{name}"));
        }
        dispatch.line("llvm.br ^i0");
        m.push_str(&dispatch.out);
        m.push_str(&l.out);
        m.push_str("  }\n}\n");
        Lowered { text: m, exits: l.exits, headers }
    }

    /// Loops: (loop header, the backward jump); the loop header is the target of the backward jump (OSR enters here).
    fn loops(func: &Function) -> Vec<(u32, u32)> {
        (0u32..)
            .zip(&func.insns)
            .filter_map(|(pc, insn)| match insn {
                Insn::Jump(l) | Insn::JumpIfTrue(l) | Insn::JumpIfFalse(l) if l.0 <= pc => Some((l.0, pc)),
                _ => None,
            })
            .collect()
    }

    fn line(&mut self, s: &str) {
        self.out.push_str("    ");
        self.out.push_str(s);
        self.out.push('\n');
    }

    fn label(&mut self, name: &str) {
        let _ = writeln!(self.out, "  ^{name}:");
    }

    fn fresh(&mut self) -> V {
        self.next += 1;
        alloc::format!("%v{}", self.next)
    }

    /// `v = <rhs>`
    fn def(&mut self, rhs: &str) -> V {
        let v = self.fresh();
        self.line(&alloc::format!("{v} = {rhs}"));
        v
    }

    fn int(&mut self, x: u64, bits: u32) -> V {
        self.def(&alloc::format!("llvm.mlir.constant({x} : i{bits}) : i{bits}"))
    }

    /// A value word as an `i64` constant (written signed: boxed words are above `i64::MAX`)
    fn word(&mut self, w: u64) -> V {
        self.def(&alloc::format!("llvm.mlir.constant({} : i64) : i64", w.cast_signed()))
    }

    fn float(&mut self, x: f64) -> V {
        // Written as a bit pattern, so `NaN` / infinities / `-0` are all preserved
        self.def(&alloc::format!("llvm.mlir.constant(0x{:016X} : f64) : f64", x.to_bits()))
    }

    /// The local holding register `r`'s value word
    fn reg_addr(r: Reg) -> V {
        alloc::format!("%r{}", r.0)
    }

    /// The local holding the accumulator's value word
    fn acc_addr() -> V {
        String::from("%a")
    }

    /// Reads a local (all stored as i64) as `ty`: `f64` is a bitcast (numbers)
    fn load(&mut self, p: &str, ty: &str) -> V {
        let w = self.def(&alloc::format!("llvm.load {p} : !llvm.ptr -> i64"));
        match ty {
            "f64" => self.def(&alloc::format!("llvm.bitcast {w} : i64 to f64")),
            _ => w,
        }
    }

    /// Writes a local: the `ty` value is stored as i64
    fn store(&mut self, v: &str, p: &str, ty: &str) {
        let w = match ty {
            "f64" => self.def(&alloc::format!("llvm.bitcast {v} : f64 to i64")),
            _ => String::from(v),
        };
        self.line(&alloc::format!("llvm.store {w}, {p} : i64, !llvm.ptr"));
    }

    /// Address of byte `at` in the frame (`%regs` / `%acc`)
    fn frame_addr(&mut self, base: &str, at: usize) -> V {
        self.def(&alloc::format!("llvm.getelementptr {base}[{at}] : (!llvm.ptr) -> !llvm.ptr, i8"))
    }

    /// All locals: (local, base address in the frame, byte offset)
    fn locals(&self) -> Vec<(String, &'static str, usize)> {
        let mut out: Vec<(String, &'static str, usize)> = (0..usize::from(self.func.regs)).map(|r| (alloc::format!("%r{r}"), "%regs", r * ValueTag::SIZE)).collect();
        out.push((String::from("%a"), "%acc", 0));
        out
    }

    /// Whether value word `w` is a number (i1): every word below the boxed range is
    fn is_number(&mut self, w: &str) -> V {
        let min = self.word(ValueBits::BOXED_MIN);
        self.icmp(ICMP_ULT, w, &min, "i64")
    }

    /// Whether value word `w` is exactly `bits` (i1)
    fn is_word(&mut self, w: &str, bits: u64) -> V {
        let c = self.word(bits);
        self.icmp(ICMP_EQ, w, &c, "i64")
    }

    fn icmp(&mut self, pred: &str, a: &str, b: &str, ty: &str) -> V {
        self.def(&alloc::format!("llvm.icmp \"{pred}\" {a}, {b} : {ty}"))
    }

    fn fcmp(&mut self, pred: &str, a: &str, b: &str) -> V {
        self.def(&alloc::format!("llvm.fcmp \"{pred}\" {a}, {b} : f64"))
    }

    /// If the condition does not hold, bails out to the interpreter (stopping at `pc`); if it holds, continues on (in a
    /// new block).
    fn guard(&mut self, pc: u32, ok: &str) {
        self.deopts.insert(pc);
        let k = self.fresh();
        let name = k.trim_start_matches('%');
        self.line(&alloc::format!("llvm.cond_br {ok}, ^k{name}, ^d{pc}"));
        self.label(&alloc::format!("k{name}"));
    }

    /// Writes number `x` to the accumulator (any NaN becomes the canonical one: a negative NaN would read as a boxed
    /// value)
    fn put_number(&mut self, x: &str) {
        let bits = self.def(&alloc::format!("llvm.bitcast {x} : f64 to i64"));
        let nan = self.fcmp(FCmp::UNO, x, x);
        let canon = self.word(ValueBits::CANONICAL_NAN);
        let w = self.def(&alloc::format!("llvm.select {nan}, {canon}, {bits} : i1, i64"));
        self.store(&w, &Self::acc_addr(), "i64");
    }

    /// Writes boolean `b` (i1) to the accumulator
    fn put_bool(&mut self, b: &str) {
        let low = self.def(&alloc::format!("llvm.zext {b} : i1 to i64"));
        let f = self.word(ValueBits::FALSE);
        let w = self.def(&alloc::format!("llvm.or {f}, {low} : i64"));
        self.store(&w, &Self::acc_addr(), "i64");
    }

    /// Writes a constant value word (undefined / null / uninitialized) to the accumulator
    fn put_word(&mut self, bits: u64) {
        let w = self.word(bits);
        self.store(&w, &Self::acc_addr(), "i64");
    }

    /// Moves a value word between two locals
    fn copy(&mut self, from: &str, to: &str) {
        let w = self.load(from, "i64");
        self.store(&w, to, "i64");
    }

    /// Register `r` and the accumulator are both numbers: gives (r's number, the accumulator's number); otherwise bails
    /// out to the interpreter.
    fn numbers(&mut self, pc: u32, r: Reg) -> (V, V) {
        let (rw, aw) = (self.load(&Self::reg_addr(r), "i64"), self.load(&Self::acc_addr(), "i64"));
        let (a, b) = (self.is_number(&rw), self.is_number(&aw));
        let both = self.def(&alloc::format!("llvm.and {a}, {b} : i1"));
        self.guard(pc, &both);
        let x = self.def(&alloc::format!("llvm.bitcast {rw} : i64 to f64"));
        let y = self.def(&alloc::format!("llvm.bitcast {aw} : i64 to f64"));
        (x, y)
    }

    /// The accumulator is a number: gives it; otherwise bails out to the interpreter.
    fn acc_number(&mut self, pc: u32) -> V {
        let w = self.load(&Self::acc_addr(), "i64");
        let ok = self.is_number(&w);
        self.guard(pc, &ok);
        self.def(&alloc::format!("llvm.bitcast {w} : i64 to f64"))
    }

    /// The local `p` is not an uninitialized binding; otherwise bails out to the interpreter.
    fn initialized(&mut self, pc: u32, p: &str) {
        let w = self.load(p, "i64");
        let u = self.word(ValueBits::UNINIT);
        let ok = self.icmp(ICMP_NE, &w, &u, "i64");
        self.guard(pc, &ok);
    }

    fn next_block(&mut self, pc: u32) {
        self.line(&alloc::format!("llvm.br ^i{}", pc + 1));
    }

    /// ToBoolean(acc), only for booleans / numbers / undefined / null; anything else bails out to the interpreter.
    fn truthy(&mut self, pc: u32) -> V {
        let w = self.load(&Self::acc_addr(), "i64");
        let is_num = self.is_number(&w);
        let one = self.word(1);
        let base = self.def(&alloc::format!("llvm.or {w}, {one} : i64"));
        let is_bool = self.is_word(&base, ValueBits::TRUE);
        let is_undef = self.is_word(&w, ValueBits::UNDEFINED);
        let is_null = self.is_word(&w, ValueBits::NULL);
        let k1 = self.def(&alloc::format!("llvm.or {is_bool}, {is_num} : i1"));
        let k2 = self.def(&alloc::format!("llvm.or {is_undef}, {is_null} : i1"));
        let known = self.def(&alloc::format!("llvm.or {k1}, {k2} : i1"));
        self.guard(pc, &known);
        let b = self.is_word(&w, ValueBits::TRUE);
        let x = self.def(&alloc::format!("llvm.bitcast {w} : i64 to f64"));
        let zero = self.float(0.0);
        // Non-zero and not NaN
        let n = self.fcmp(FCmp::ONE, &x, &zero);
        let v = self.def(&alloc::format!("llvm.select {is_num}, {n}, {b} : i1, i1"));
        let f = self.int(0, 1);
        self.def(&alloc::format!("llvm.select {k1}, {v}, {f} : i1, i1"))
    }

    /// `return`: a function entered at the start returns (exit code 0); one entered midway through a loop (OSR) needs
    /// the interpreter to finish the frame, so it stops before this `return`.
    fn ret(&mut self, pc: u32) {
        self.deopts.insert(pc);
        let z = self.int(0, 64);
        let whole = self.icmp(ICMP_EQ, "%start", &z, "i64");
        let ret = self.fresh();
        let name = ret.trim_start_matches('%');
        self.line(&alloc::format!("llvm.cond_br {whole}, ^r{name}, ^d{pc}"));
        self.label(&alloc::format!("r{name}"));
        self.line(&alloc::format!("llvm.br ^x({z} : i64)"));
    }

    /// One instruction; a fused instruction is lowered as the original instructions it splits back into, in order (same
    /// `pc`: bailing out midway redoes the whole thing, so the earlier parts only write acc).
    fn insn(&mut self, pc: u32, insn: Insn) -> Result<(), Unsupported> {
        let Some(parts) = insn.unfused() else { return self.step(pc, insn, true) };
        let (last, init) = parts.split_last().expect("a fused instruction has parts");
        for &p in init {
            self.step(pc, p, false)?;
        }
        self.step(pc, *last, true)
    }

    /// Lowers one original instruction; when `last` is false it does not link to the next block (the middle parts of a
    /// fused instruction).
    fn step(&mut self, pc: u32, insn: Insn, last: bool) -> Result<(), Unsupported> {
        let unsupported = Unsupported { pc, insn };
        match insn {
            Insn::LdaUndefined => self.put_word(ValueBits::UNDEFINED),
            Insn::LdaNull => self.put_word(ValueBits::NULL),
            Insn::LdaHole => self.put_word(ValueBits::UNINIT),
            Insn::LdaBool(b) => {
                let v = self.int(u64::from(b), 1);
                self.put_bool(&v);
            }
            Insn::LdaSmi(n) => {
                let v = self.float(f64::from(n));
                self.put_number(&v);
            }
            Insn::LdaConst(k) => match self.func.consts.get(k.0 as usize) {
                Some(Constant::Number(x)) => {
                    let v = self.float(*x);
                    self.put_number(&v);
                }
                _ => return Err(unsupported),
            },
            Insn::Ldar(r) => self.copy(&Self::reg_addr(r), &Self::acc_addr()),
            Insn::Star(r) => self.copy(&Self::acc_addr(), &Self::reg_addr(r)),
            Insn::Binary(op, r) => {
                let (a, b) = self.numbers(pc, r);
                let v = match op {
                    BinaryOp::Add => self.def(&alloc::format!("llvm.fadd {a}, {b} : f64")),
                    BinaryOp::Sub => self.def(&alloc::format!("llvm.fsub {a}, {b} : f64")),
                    BinaryOp::Mul => self.def(&alloc::format!("llvm.fmul {a}, {b} : f64")),
                    BinaryOp::Div => self.def(&alloc::format!("llvm.fdiv {a}, {b} : f64")),
                    op => {
                        let sym = Helper::Number(op).symbol().ok_or(unsupported)?;
                        self.helpers.insert(sym);
                        self.def(&alloc::format!("llvm.call @{sym}({a}, {b}) : (f64, f64) -> f64"))
                    }
                };
                self.put_number(&v);
            }
            Insn::Compare(op, r) => {
                let (a, b) = self.numbers(pc, r);
                let c = self.fcmp(FCmp::of(op), &a, &b);
                self.put_bool(&c);
            }
            Insn::ToNumeric => {
                self.acc_number(pc);
            }
            Insn::Inc | Insn::Dec => {
                let x = self.acc_number(pc);
                let one = self.float(1.0);
                let op = if matches!(insn, Insn::Inc) { "fadd" } else { "fsub" };
                let v = self.def(&alloc::format!("llvm.{op} {x}, {one} : f64"));
                self.put_number(&v);
            }
            Insn::CheckInit { .. } => self.initialized(pc, &Self::acc_addr()),
            Insn::CheckReg { reg, .. } => self.initialized(pc, &Self::reg_addr(reg)),
            Insn::Jump(l) => {
                self.line(&alloc::format!("llvm.br ^i{}", l.0));
                return Ok(());
            }
            Insn::JumpIfTrue(l) | Insn::JumpIfFalse(l) => {
                let v = self.truthy(pc);
                let (yes, no) = (alloc::format!("^i{}", l.0), alloc::format!("^i{}", pc + 1));
                let (t, f) = if matches!(insn, Insn::JumpIfTrue(_)) { (yes, no) } else { (no, yes) };
                self.line(&alloc::format!("llvm.cond_br {v}, {t}, {f}"));
                return Ok(());
            }
            Insn::Return => {
                self.ret(pc);
                return Ok(());
            }
            _ => return Err(unsupported),
        }
        if !last {
            return Ok(());
        }
        if pc as usize + 1 >= self.func.insns.len() {
            // The last instruction falls off the end of the function: shouldn't happen until that is handled as
            // returning undefined; bail out to the interpreter
            self.deopts.insert(pc + 1);
            self.line(&alloc::format!("llvm.br ^d{}", pc + 1));
            return Ok(());
        }
        self.next_block(pc);
        Ok(())
    }
}
