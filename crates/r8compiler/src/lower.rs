use alloc::collections::BTreeSet;
use alloc::string::String;
use core::fmt::Write;

use r8bytecode::{Constant, Function, Insn, Reg};
use r8frame::{BinaryOp, CompareOp, ValueTag};

use crate::Helper;

/// 机器码入口函数的符号名
pub(crate) const ENTRY: &str = "r8_jit_entry";

/// 第一档不收的指令（整函数不编）：第几条、是什么。
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Unsupported {
    pub pc: u32,
    pub insn: Insn,
}

/// `llvm.fcmp` 的谓词（自定义写法里的名字）
struct FCmp;

impl FCmp {
    const OEQ: &str = "oeq";
    const OGT: &str = "ogt";
    const OGE: &str = "oge";
    const OLT: &str = "olt";
    const OLE: &str = "ole";
    /// 有序且不等（`NaN` 给假）
    const ONE: &str = "one";
    /// 无序或不等（`NaN` 给真，`!=` 的语义）
    const UNE: &str = "une";

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

/// `llvm.icmp` 的谓词
const ICMP_EQ: &str = "eq";
const ICMP_NE: &str = "ne";

/// 一个字节码函数 ⇒ MLIR 文本（`llvm` 方言）。
///
/// 每条指令一个块 `^iN`；要退回解释器的指令另有 `^dN`（返回 `N + 1`）。累加器与寄存器都在内存里
/// （`%acc` / `%regs` 指向的 [`Value`](r8frame::Value)），每条指令读写它们，块之间不传值。
pub(crate) struct Lower<'f> {
    func: &'f Function,
    out: String,
    next: u32,
    /// 用到退出块的指令
    deopts: BTreeSet<u32>,
    /// 用到的运行时函数
    helpers: BTreeSet<&'static str>,
}

/// 一个 SSA 值的名字
type V = String;

impl<'f> Lower<'f> {
    /// 整个模块：运行时函数的声明 + 入口函数。
    pub(crate) fn module(func: &'f Function) -> Result<String, Unsupported> {
        let mut l = Lower { func, out: String::new(), next: 0, deopts: BTreeSet::new(), helpers: BTreeSet::new() };
        for (pc, &insn) in (0u32..).zip(&func.insns) {
            l.label(&alloc::format!("i{pc}"));
            l.insn(pc, insn)?;
        }
        if func.insns.is_empty() {
            l.label("i0");
            l.line("%r = llvm.mlir.constant(0 : i64) : i64");
            l.line("llvm.return %r : i64");
        }
        for pc in core::mem::take(&mut l.deopts) {
            l.label(&alloc::format!("d{pc}"));
            let c = l.int(u64::from(pc) + 1, 64);
            l.line(&alloc::format!("llvm.return {c} : i64"));
        }
        let mut m = String::from("module {\n");
        for h in &l.helpers {
            let _ = writeln!(m, "  llvm.func @{h}(f64, f64) -> f64");
        }
        let _ = writeln!(m, "  llvm.func @{ENTRY}(%regs: !llvm.ptr, %acc: !llvm.ptr) -> i64 {{");
        m.push_str("    llvm.br ^i0\n");
        m.push_str(&l.out);
        m.push_str("  }\n}\n");
        Ok(m)
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

    fn float(&mut self, x: f64) -> V {
        // 位模式写，`NaN` / 无穷 / `-0` 都原样
        self.def(&alloc::format!("llvm.mlir.constant(0x{:016X} : f64) : f64", x.to_bits()))
    }

    /// 寄存器 `r` 的值（`%regs` 里第 r 个）第 `off` 字节的地址
    fn reg_addr(&mut self, r: Reg, off: usize) -> V {
        let at = usize::from(r.0) * ValueTag::SIZE + off;
        self.def(&alloc::format!("llvm.getelementptr %regs[{at}] : (!llvm.ptr) -> !llvm.ptr, i8"))
    }

    /// 累加器第 `off` 字节的地址
    fn acc_addr(&mut self, off: usize) -> V {
        self.def(&alloc::format!("llvm.getelementptr %acc[{off}] : (!llvm.ptr) -> !llvm.ptr, i8"))
    }

    fn load(&mut self, p: &str, ty: &str) -> V {
        self.def(&alloc::format!("llvm.load {p} : !llvm.ptr -> {ty}"))
    }

    fn store(&mut self, v: &str, p: &str, ty: &str) {
        self.line(&alloc::format!("llvm.store {v}, {p} : {ty}, !llvm.ptr"));
    }

    /// 地址 `p` 处的值是不是标签 `tag`（i1）
    fn tag_is(&mut self, p: &str, tag: u8) -> V {
        let t = self.load(p, "i8");
        let c = self.int(u64::from(tag), 8);
        self.icmp(ICMP_EQ, &t, &c, "i8")
    }

    fn icmp(&mut self, pred: &str, a: &str, b: &str, ty: &str) -> V {
        self.def(&alloc::format!("llvm.icmp \"{pred}\" {a}, {b} : {ty}"))
    }

    fn fcmp(&mut self, pred: &str, a: &str, b: &str) -> V {
        self.def(&alloc::format!("llvm.fcmp \"{pred}\" {a}, {b} : f64"))
    }

    /// 条件不成立就退回解释器（停在 `pc`），成立接着往下（新开一个块）。
    fn guard(&mut self, pc: u32, ok: &str) {
        self.deopts.insert(pc);
        let k = self.fresh();
        let name = k.trim_start_matches('%');
        self.line(&alloc::format!("llvm.cond_br {ok}, ^k{name}, ^d{pc}"));
        self.label(&alloc::format!("k{name}"));
    }

    /// 累加器写成数 `x`
    fn put_number(&mut self, x: &str) {
        let (tp, pp) = (self.acc_addr(0), self.acc_addr(ValueTag::PAYLOAD));
        let tag = self.int(u64::from(ValueTag::NUMBER), 64);
        self.store(&tag, &tp, "i64");
        self.store(x, &pp, "f64");
    }

    /// 累加器写成布尔 `b`（i1）
    fn put_bool(&mut self, b: &str) {
        let (tp, pp) = (self.acc_addr(0), self.acc_addr(ValueTag::PAYLOAD));
        let tag = self.int(u64::from(ValueTag::BOOL), 64);
        self.store(&tag, &tp, "i64");
        let byte = self.def(&alloc::format!("llvm.zext {b} : i1 to i8"));
        self.store(&byte, &pp, "i8");
    }

    /// 累加器写成只有标签的值（undefined / null / 未初始化）
    fn put_tag(&mut self, tag: u8) {
        let tp = self.acc_addr(0);
        let t = self.int(u64::from(tag), 64);
        self.store(&t, &tp, "i64");
    }

    /// 两个 16 字节的值之间搬（标签字与载荷字）
    fn copy(&mut self, from: &(V, V), to: &(V, V)) {
        let (a, b) = (self.load(&from.0, "i64"), self.load(&from.1, "i64"));
        self.store(&a, &to.0, "i64");
        self.store(&b, &to.1, "i64");
    }

    /// 寄存器 `r` 与累加器都是数：给出（r 的数, 累加器的数），不是就退回解释器。
    fn numbers(&mut self, pc: u32, r: Reg) -> (V, V) {
        let (rt, at) = (self.reg_addr(r, 0), self.acc_addr(0));
        let (a, b) = (self.tag_is(&rt, ValueTag::NUMBER), self.tag_is(&at, ValueTag::NUMBER));
        let both = self.def(&alloc::format!("llvm.and {a}, {b} : i1"));
        self.guard(pc, &both);
        let (rp, ap) = (self.reg_addr(r, ValueTag::PAYLOAD), self.acc_addr(ValueTag::PAYLOAD));
        (self.load(&rp, "f64"), self.load(&ap, "f64"))
    }

    /// 累加器是数：给出它，不是就退回解释器。
    fn acc_number(&mut self, pc: u32) -> V {
        let at = self.acc_addr(0);
        let ok = self.tag_is(&at, ValueTag::NUMBER);
        self.guard(pc, &ok);
        let ap = self.acc_addr(ValueTag::PAYLOAD);
        self.load(&ap, "f64")
    }

    fn next_block(&mut self, pc: u32) {
        self.line(&alloc::format!("llvm.br ^i{}", pc + 1));
    }

    /// ToBoolean(acc)，只认布尔 / 数 / undefined / null，别的退回解释器。
    fn truthy(&mut self, pc: u32) -> V {
        let at = self.acc_addr(0);
        let is_bool = self.tag_is(&at, ValueTag::BOOL);
        let is_num = self.tag_is(&at, ValueTag::NUMBER);
        let is_undef = self.tag_is(&at, ValueTag::UNDEFINED);
        let is_null = self.tag_is(&at, ValueTag::NULL);
        let k1 = self.def(&alloc::format!("llvm.or {is_bool}, {is_num} : i1"));
        let k2 = self.def(&alloc::format!("llvm.or {is_undef}, {is_null} : i1"));
        let known = self.def(&alloc::format!("llvm.or {k1}, {k2} : i1"));
        self.guard(pc, &known);
        let pp = self.acc_addr(ValueTag::PAYLOAD);
        let byte = self.load(&pp, "i8");
        let zero8 = self.int(0, 8);
        let b = self.icmp(ICMP_NE, &byte, &zero8, "i8");
        let x = self.load(&pp, "f64");
        let zero = self.float(0.0);
        // 非零且不是 NaN
        let n = self.fcmp(FCmp::ONE, &x, &zero);
        let v = self.def(&alloc::format!("llvm.select {is_num}, {n}, {b} : i1, i1"));
        let f = self.int(0, 1);
        self.def(&alloc::format!("llvm.select {k1}, {v}, {f} : i1, i1"))
    }

    fn insn(&mut self, pc: u32, insn: Insn) -> Result<(), Unsupported> {
        let unsupported = Unsupported { pc, insn };
        match insn {
            Insn::LdaUndefined => self.put_tag(ValueTag::UNDEFINED),
            Insn::LdaNull => self.put_tag(ValueTag::NULL),
            Insn::LdaHole => self.put_tag(ValueTag::UNINIT),
            Insn::LdaBool(b) => {
                let v = self.int(u64::from(b), 1);
                self.put_bool(&v);
            }
            Insn::LdaConst(k) => match self.func.consts.get(k.0 as usize) {
                Some(Constant::Number(x)) => {
                    let v = self.float(*x);
                    self.put_number(&v);
                }
                _ => return Err(unsupported),
            },
            Insn::Ldar(r) => {
                let from = (self.reg_addr(r, 0), self.reg_addr(r, ValueTag::PAYLOAD));
                let to = (self.acc_addr(0), self.acc_addr(ValueTag::PAYLOAD));
                self.copy(&from, &to);
            }
            Insn::Star(r) => {
                let from = (self.acc_addr(0), self.acc_addr(ValueTag::PAYLOAD));
                let to = (self.reg_addr(r, 0), self.reg_addr(r, ValueTag::PAYLOAD));
                self.copy(&from, &to);
            }
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
            Insn::CheckInit { .. } => {
                let at = self.acc_addr(0);
                let t = self.load(&at, "i8");
                let u = self.int(u64::from(ValueTag::UNINIT), 8);
                let ok = self.icmp(ICMP_NE, &t, &u, "i8");
                self.guard(pc, &ok);
            }
            Insn::CheckReg { reg, .. } => {
                let at = self.reg_addr(reg, 0);
                let t = self.load(&at, "i8");
                let u = self.int(u64::from(ValueTag::UNINIT), 8);
                let ok = self.icmp(ICMP_NE, &t, &u, "i8");
                self.guard(pc, &ok);
            }
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
                let z = self.int(0, 64);
                self.line(&alloc::format!("llvm.return {z} : i64"));
                return Ok(());
            }
            _ => return Err(unsupported),
        }
        if pc as usize + 1 >= self.func.insns.len() {
            // 最后一条指令落到函数末尾：按返回 undefined 处理之前不该发生，退回解释器
            self.deopts.insert(pc + 1);
            self.line(&alloc::format!("llvm.br ^d{}", pc + 1));
            return Ok(());
        }
        self.next_block(pc);
        Ok(())
    }
}
