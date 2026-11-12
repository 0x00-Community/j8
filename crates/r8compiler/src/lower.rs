use alloc::collections::BTreeSet;
use alloc::string::String;
use alloc::vec::Vec;
use core::fmt::Write;

use r8virtasm::{Constant, Function, Insn, Reg};
use r8frame::{BinaryOp, CompareOp, ValueTag};

use crate::Helper;

/// 机器码入口函数的符号名
pub(crate) const ENTRY: &str = "r8_jit_entry";

/// 第一档不收的指令（机器码停在它之前，交给解释器执行）：第几条、是什么。
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
/// 每条指令一个块 `^iN`；要退回解释器的指令另有 `^dN`（返回 `N + 1`）。累加器与每个寄存器是一对局部变量
/// （`llvm.alloca`：标签、载荷）：入口从帧（`%acc` / `%regs` 指向的 [`Value`](r8frame::Value)）读进来，
/// 所有出口都跳到写回块 `^x` 写回帧再返回；局部变量之后由 mem2reg 提成 SSA。
pub(crate) struct Lower<'f> {
    func: &'f Function,
    out: String,
    next: u32,
    /// 用到退出块的指令
    deopts: BTreeSet<u32>,
    /// 用到的运行时函数
    helpers: BTreeSet<&'static str>,
    /// 第一档不收、停下交给解释器的指令
    exits: Vec<Unsupported>,
}

/// 翻好的模块与它的概况。
pub(crate) struct Lowered {
    pub(crate) text: String,
    pub(crate) exits: Vec<Unsupported>,
    pub(crate) headers: Vec<u32>,
}

/// 一个 SSA 值的名字
type V = String;

impl<'f> Lower<'f> {
    /// 整个模块：运行时函数的声明 + 入口函数。
    pub(crate) fn module(func: &'f Function) -> Lowered {
        let mut l = Lower { func, out: String::new(), next: 0, deopts: BTreeSet::new(), helpers: BTreeSet::new(), exits: Vec::new() };
        let loops = Self::loops(func);
        for (pc, &insn) in (0u32..).zip(&func.insns) {
            l.label(&alloc::format!("i{pc}"));
            if let Err(u) = l.insn(pc, insn) {
                // 第一档不收的指令：停在它之前，交给解释器执行
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
        // 收尾块：局部变量一次写回帧里，带着出口码返回
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
        // 能从中途进的循环：循环体里没有停下交给解释器的指令（否则每圈都要进出一次，不划算）
        let mut headers: Vec<u32> = loops.iter().filter(|&&(h, back)| !l.exits.iter().any(|u| (h..=back).contains(&u.pc))).map(|&(h, _)| h).collect();
        headers.sort_unstable();
        headers.dedup();
        // 入口分派：`%start` 是 0 从头进，`h + 1` 从循环头 `h` 进（OSR）
        let mut dispatch = Lower { func, out: String::new(), next: l.next, deopts: BTreeSet::new(), helpers: BTreeSet::new(), exits: Vec::new() };
        // 局部变量（mem2reg 提成 SSA），从帧里读一次
        dispatch.line("%one = llvm.mlir.constant(1 : i64) : i64");
        for (local, base, at) in dispatch.locals() {
            // 通用写法：mlirformat 还读不懂 `llvm.alloca` 的自定义写法
            dispatch.line(&alloc::format!("{local} = \"llvm.alloca\"(%one) <{{elem_type = i64}}> : (i64) -> !llvm.ptr"));
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

    /// 循环：（循环头, 往回跳的那条），循环头是往回跳的目标（OSR 从这里进）。
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

    fn float(&mut self, x: f64) -> V {
        // 位模式写，`NaN` / 无穷 / `-0` 都原样
        self.def(&alloc::format!("llvm.mlir.constant(0x{:016X} : f64) : f64", x.to_bits()))
    }

    /// 寄存器 `r` 的标签字（`off` 0）或载荷字（`off` = 载荷偏移）所在的局部变量
    fn reg_addr(r: Reg, off: usize) -> V {
        if off == 0 { alloc::format!("%rt{}", r.0) } else { alloc::format!("%rp{}", r.0) }
    }

    /// 累加器的标签字 / 载荷字所在的局部变量
    fn acc_addr(off: usize) -> V {
        String::from(if off == 0 { "%at" } else { "%ap" })
    }

    /// 读局部变量（都按 i64 存）成 `ty`：`i8` 取低字节（标签、布尔），`f64` 按位换（数）
    fn load(&mut self, p: &str, ty: &str) -> V {
        let w = self.def(&alloc::format!("llvm.load {p} : !llvm.ptr -> i64"));
        match ty {
            "i8" => self.def(&alloc::format!("llvm.trunc {w} : i64 to i8")),
            "f64" => self.def(&alloc::format!("llvm.bitcast {w} : i64 to f64")),
            _ => w,
        }
    }

    /// 写局部变量：`ty` 的值按 i64 存
    fn store(&mut self, v: &str, p: &str, ty: &str) {
        let w = match ty {
            "i8" => self.def(&alloc::format!("llvm.zext {v} : i8 to i64")),
            "f64" => self.def(&alloc::format!("llvm.bitcast {v} : f64 to i64")),
            _ => String::from(v),
        };
        self.line(&alloc::format!("llvm.store {w}, {p} : i64, !llvm.ptr"));
    }

    /// 帧里（`%regs` / `%acc`）第 `at` 字节的地址
    fn frame_addr(&mut self, base: &str, at: usize) -> V {
        self.def(&alloc::format!("llvm.getelementptr {base}[{at}] : (!llvm.ptr) -> !llvm.ptr, i8"))
    }

    /// 全部局部变量：（局部变量, 帧里的基址, 字节偏移）
    fn locals(&self) -> Vec<(String, &'static str, usize)> {
        let mut out = Vec::new();
        for r in 0..usize::from(self.func.regs) {
            out.push((alloc::format!("%rt{r}"), "%regs", r * ValueTag::SIZE));
            out.push((alloc::format!("%rp{r}"), "%regs", r * ValueTag::SIZE + ValueTag::PAYLOAD));
        }
        out.push((String::from("%at"), "%acc", 0));
        out.push((String::from("%ap"), "%acc", ValueTag::PAYLOAD));
        out
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
        let (tp, pp) = (Self::acc_addr(0), Self::acc_addr(ValueTag::PAYLOAD));
        let tag = self.int(u64::from(ValueTag::NUMBER), 64);
        self.store(&tag, &tp, "i64");
        self.store(x, &pp, "f64");
    }

    /// 累加器写成布尔 `b`（i1）
    fn put_bool(&mut self, b: &str) {
        let (tp, pp) = (Self::acc_addr(0), Self::acc_addr(ValueTag::PAYLOAD));
        let tag = self.int(u64::from(ValueTag::BOOL), 64);
        self.store(&tag, &tp, "i64");
        let byte = self.def(&alloc::format!("llvm.zext {b} : i1 to i8"));
        self.store(&byte, &pp, "i8");
    }

    /// 累加器写成只有标签的值（undefined / null / 未初始化）
    fn put_tag(&mut self, tag: u8) {
        let tp = Self::acc_addr(0);
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
        let (rt, at) = (Self::reg_addr(r, 0), Self::acc_addr(0));
        let (a, b) = (self.tag_is(&rt, ValueTag::NUMBER), self.tag_is(&at, ValueTag::NUMBER));
        let both = self.def(&alloc::format!("llvm.and {a}, {b} : i1"));
        self.guard(pc, &both);
        let (rp, ap) = (Self::reg_addr(r, ValueTag::PAYLOAD), Self::acc_addr(ValueTag::PAYLOAD));
        (self.load(&rp, "f64"), self.load(&ap, "f64"))
    }

    /// 累加器是数：给出它，不是就退回解释器。
    fn acc_number(&mut self, pc: u32) -> V {
        let at = Self::acc_addr(0);
        let ok = self.tag_is(&at, ValueTag::NUMBER);
        self.guard(pc, &ok);
        let ap = Self::acc_addr(ValueTag::PAYLOAD);
        self.load(&ap, "f64")
    }

    fn next_block(&mut self, pc: u32) {
        self.line(&alloc::format!("llvm.br ^i{}", pc + 1));
    }

    /// ToBoolean(acc)，只认布尔 / 数 / undefined / null，别的退回解释器。
    fn truthy(&mut self, pc: u32) -> V {
        let at = Self::acc_addr(0);
        let is_bool = self.tag_is(&at, ValueTag::BOOL);
        let is_num = self.tag_is(&at, ValueTag::NUMBER);
        let is_undef = self.tag_is(&at, ValueTag::UNDEFINED);
        let is_null = self.tag_is(&at, ValueTag::NULL);
        let k1 = self.def(&alloc::format!("llvm.or {is_bool}, {is_num} : i1"));
        let k2 = self.def(&alloc::format!("llvm.or {is_undef}, {is_null} : i1"));
        let known = self.def(&alloc::format!("llvm.or {k1}, {k2} : i1"));
        self.guard(pc, &known);
        let pp = Self::acc_addr(ValueTag::PAYLOAD);
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

    /// `return`：从头进的函数返回（出口码 0）；从循环中途进的（OSR）帧要解释器收尾，停在这条 `return` 之前。
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
                let from = (Self::reg_addr(r, 0), Self::reg_addr(r, ValueTag::PAYLOAD));
                let to = (Self::acc_addr(0), Self::acc_addr(ValueTag::PAYLOAD));
                self.copy(&from, &to);
            }
            Insn::Star(r) => {
                let from = (Self::acc_addr(0), Self::acc_addr(ValueTag::PAYLOAD));
                let to = (Self::reg_addr(r, 0), Self::reg_addr(r, ValueTag::PAYLOAD));
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
                let at = Self::acc_addr(0);
                let t = self.load(&at, "i8");
                let u = self.int(u64::from(ValueTag::UNINIT), 8);
                let ok = self.icmp(ICMP_NE, &t, &u, "i8");
                self.guard(pc, &ok);
            }
            Insn::CheckReg { reg, .. } => {
                let at = Self::reg_addr(reg, 0);
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
                self.ret(pc);
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
