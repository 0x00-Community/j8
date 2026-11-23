use alloc::format;
use alloc::string::String;
use alloc::vec;
use alloc::vec::Vec;
use core::fmt::Write;

use j8executor::FrameSlots;
use j8framework::{Layout, Value, ValueBits};
use mlirpass::{HoleKind, Stencil, Stencils};

use crate::asm::{Asm, Label, RAX, RCX, RDI, RDX, RSI, R15, R8, R9, RBP};
use crate::build::{Kind, OBJECT_TAG};
use crate::codegen::{Exit, Generated, Reason, Sym, RETURNED};
use crate::ir::{Cond, FloatOp, Graph, Helper, IntOp, Op, Rep, Slot, B, Fs, V};
use crate::runtime::{Ctx, Ret};

#[derive(Debug)]
pub struct Unsupported(pub String);

type Stitching<T> = Result<T, Unsupported>;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum St {
    Int(IntOp, bool),
    Float(FloatOp),
    IntToFloat,
    BoxFloat,
    BoxBool,
    BoxPtr,
    Untag,
    UnboxInt,
    FloatToInt,
    UnboxFloat,
    CheckObject,
    CheckNotHole,
    CheckNotLazy,
    CheckFunc,
    CheckShape(bool),
    Check(bool),
    CmpInt(Cond),
    CmpWord(Cond),
    CmpFloat(Cond),
    LoadWord(bool),
    StoreCell,
    StoreCellAddr,
    ContextCell,
    Poll,
    Global,
    Enter,
    Helper(u8, bool, bool),
    HelperMany(bool, bool),
    CallJit(u8),
}

const INT_OPS: [IntOp; 10] = [IntOp::Add, IntOp::Sub, IntOp::Mul, IntOp::Mod, IntOp::And, IntOp::Or, IntOp::Xor, IntOp::Shl, IntOp::Sar, IntOp::Shr];
const FLOAT_OPS: [FloatOp; 4] = [FloatOp::Add, FloatOp::Sub, FloatOp::Mul, FloatOp::Div];
const CONDS: [Cond; 6] = [Cond::Lt, Cond::Le, Cond::Gt, Cond::Ge, Cond::Eq, Cond::Ne];
const HELPER_REGS: u8 = 4;
const JIT_ARGS: u8 = 3;

impl St {
    fn all() -> Vec<St> {
        let mut out = Vec::new();
        for op in INT_OPS {
            if op != IntOp::Mod {
                out.push(St::Int(op, false));
            }
            if matches!(op, IntOp::Add | IntOp::Sub | IntOp::Mul | IntOp::Mod | IntOp::Shr) {
                out.push(St::Int(op, true));
            }
        }
        out.extend(FLOAT_OPS.map(St::Float));
        out.extend([St::IntToFloat, St::BoxFloat, St::BoxBool, St::BoxPtr, St::Untag, St::UnboxInt, St::FloatToInt, St::UnboxFloat, St::CheckObject, St::CheckNotHole, St::CheckNotLazy, St::CheckFunc, St::CheckShape(false), St::CheckShape(true), St::Check(false), St::Check(true)]);
        for c in CONDS {
            out.extend([St::CmpInt(c), St::CmpWord(c), St::CmpFloat(c)]);
        }
        out.extend([St::LoadWord(false), St::LoadWord(true), St::StoreCell, St::StoreCellAddr, St::ContextCell, St::Poll, St::Global, St::Enter]);
        for throws in [false, true] {
            for result in [false, true] {
                out.extend((0..=HELPER_REGS).map(|n| St::Helper(n, throws, result)));
                out.push(St::HelperMany(throws, result));
            }
        }
        out.extend((0..=JIT_ARGS).map(St::CallJit));
        out
    }

    fn name(self) -> String {
        format!("{self:?}").replace(['(', ')', ',', ' '], "_")
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
    fn fresh(&mut self) -> String {
        self.n += 1;
        format!("%v{}", self.n)
    }

    fn line(&mut self, text: &str) -> String {
        let v = self.fresh();
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

    fn slot_ptr(&mut self, k: &str) -> String {
        let o = self.line(&format!("llvm.call @slot.{k}() : () -> i64"));
        let p = self.line(&format!("llvm.add %fp, {o} : i64"));
        self.line(&format!("llvm.inttoptr {p} : i64 to !llvm.ptr"))
    }

    fn read(&mut self, k: u8, ty: &str) -> String {
        let p = self.slot_ptr(&format!("{k}"));
        self.line(&format!("llvm.load {p} : !llvm.ptr -> {ty}"))
    }

    fn write(&mut self, v: &str, ty: &str) {
        let p = self.slot_ptr("d");
        self.stmt(&format!("llvm.store {v}, {p} : {ty}, !llvm.ptr"));
    }

    fn write_i32(&mut self, v: &str) {
        let w = self.line(&format!("llvm.sext {v} : i32 to i64"));
        self.write(&w, "i64");
    }

    fn write_bool(&mut self, v: &str) {
        let w = self.line(&format!("llvm.zext {v} : i1 to i64"));
        self.write(&w, "i64");
    }

    fn at(&mut self, base: &str, off: u64) -> String {
        let a = if off == 0 {
            String::from(base)
        } else {
            let o = self.c64(off);
            self.line(&format!("llvm.add {base}, {o} : i64"))
        };
        self.line(&format!("llvm.inttoptr {a} : i64 to !llvm.ptr"))
    }

    fn load(&mut self, base: &str, off: u64, ty: &str) -> String {
        let p = self.at(base, off);
        self.line(&format!("llvm.load {p} : !llvm.ptr -> {ty}"))
    }

    fn field(off: i32) -> u64 {
        u64::from(off.unsigned_abs())
    }

    fn off(x: usize) -> u64 {
        u64::try_from(x).unwrap_or(u64::MAX)
    }

    fn label(&mut self) -> String {
        self.blocks += 1;
        format!("^b{}", self.blocks)
    }

    fn exit_if(&mut self, c: &str, jump: u8) {
        let (fail, ok) = (self.label(), self.label());
        self.stmt(&format!("llvm.cond_br {c}, {fail}, {ok}"));
        let _ = writeln!(self.out, "  {fail}:\n    llvm.call @jump.{jump}() : () -> ()\n    llvm.unreachable\n  {ok}:");
    }

    fn icmp(&mut self, p: &str, a: &str, b: &str, ty: &str) -> String {
        self.line(&format!("llvm.icmp \"{p}\" {a}, {b} : {ty}"))
    }

    fn icmp64(&mut self, p: &str, a: &str, b: u64) -> String {
        let c = self.c64(b);
        self.icmp(p, a, &c, "i64")
    }

    fn bin(&mut self, op: &str, a: &str, b: &str, ty: &str) -> String {
        self.line(&format!("llvm.{op} {a}, {b} : {ty}"))
    }

    fn untag(&mut self, x: &str) -> String {
        let m = self.c64(ValueBits::PAYLOAD);
        self.bin("and", x, &m, "i64")
    }

    fn pending(&mut self, jump: u8) {
        let p = self.load("%ctx", Self::field(Ctx::PENDING), "i64");
        let t = self.icmp64("ne", &p, 0);
        self.exit_if(&t, jump);
    }

    fn call(&mut self, callee: &str, args: &[String]) -> String {
        let tys = vec!["i64"; args.len()].join(", ");
        self.line(&format!("llvm.call @{callee}({}) : ({tys}) -> i64", args.join(", ")))
    }

    fn integral(&mut self, f: &str, raw: &str, jump: u8) -> String {
        let i = self.line(&format!("llvm.fptosi {f} : f64 to i32"));
        let back = self.line(&format!("llvm.sitofp {i} : i32 to f64"));
        let inexact = self.line(&format!("llvm.fcmp \"une\" {f}, {back} : f64"));
        self.exit_if(&inexact, jump);
        let zero = self.c32(0);
        let z = self.icmp("eq", &i, &zero, "i32");
        let signed = self.icmp64("ne", raw, 0);
        let minus = self.bin("and", &z, &signed, "i1");
        self.exit_if(&minus, jump);
        i
    }

    fn signed(c: Cond) -> &'static str {
        match c {
            Cond::Lt => "slt",
            Cond::Le => "sle",
            Cond::Gt => "sgt",
            Cond::Ge => "sge",
            Cond::Eq => "eq",
            Cond::Ne => "ne",
        }
    }

    fn build(&mut self, st: St) {
        match st {
            St::Int(op, checked) => self.int(op, checked),
            St::Float(op) => {
                let (a, b) = (self.read(0, "f64"), self.read(1, "f64"));
                let name = match op {
                    FloatOp::Add => "fadd",
                    FloatOp::Sub => "fsub",
                    FloatOp::Mul => "fmul",
                    FloatOp::Div => "fdiv",
                };
                let r = self.bin(name, &a, &b, "f64");
                self.write(&r, "f64");
            }
            St::IntToFloat => {
                let a = self.read(0, "i32");
                let f = self.line(&format!("llvm.sitofp {a} : i32 to f64"));
                self.write(&f, "f64");
            }
            St::BoxFloat => {
                let f = self.read(0, "f64");
                let bits = self.line(&format!("llvm.bitcast {f} : f64 to i64"));
                let nan = self.line(&format!("llvm.fcmp \"uno\" {f}, {f} : f64"));
                let canon = self.c64(ValueBits::CANONICAL_NAN);
                let r = self.line(&format!("llvm.select {nan}, {canon}, {bits} : i1, i64"));
                self.write(&r, "i64");
            }
            St::BoxBool => {
                let b = self.read(0, "i64");
                let f = self.c64(ValueBits::FALSE);
                let r = self.bin("add", &f, &b, "i64");
                self.write(&r, "i64");
            }
            St::BoxPtr => {
                let x = self.read(0, "i64");
                let tag = self.hole(0);
                let r = self.bin("or", &x, &tag, "i64");
                self.write(&r, "i64");
            }
            St::Untag => {
                let x = self.read(0, "i64");
                let r = self.untag(&x);
                self.write(&r, "i64");
            }
            St::UnboxInt => {
                let x = self.read(0, "i64");
                let not = self.icmp64("uge", &x, ValueBits::BOXED_MIN);
                self.exit_if(&not, 0);
                let f = self.line(&format!("llvm.bitcast {x} : i64 to f64"));
                let i = self.integral(&f, &x, 1);
                self.write_i32(&i);
            }
            St::FloatToInt => {
                let f = self.read(0, "f64");
                let raw = self.line(&format!("llvm.bitcast {f} : f64 to i64"));
                let i = self.integral(&f, &raw, 0);
                self.write_i32(&i);
            }
            St::UnboxFloat => {
                let x = self.read(0, "i64");
                let not = self.icmp64("uge", &x, ValueBits::BOXED_MIN);
                self.exit_if(&not, 0);
                self.write(&x, "i64");
            }
            St::CheckObject => {
                let x = self.read(0, "i64");
                let s = self.c64(u64::from(ValueBits::TAG_SHIFT));
                let tag = self.bin("lshr", &x, &s, "i64");
                let bad = self.icmp64("ne", &tag, OBJECT_TAG >> ValueBits::TAG_SHIFT);
                self.exit_if(&bad, 0);
                let r = self.untag(&x);
                self.write(&r, "i64");
            }
            St::CheckNotHole => {
                let x = self.read(0, "i64");
                let hole = self.icmp64("eq", &x, ValueBits::UNINIT);
                self.exit_if(&hole, 0);
            }
            St::CheckNotLazy => {
                let x = self.read(0, "i64");
                let s = self.c64(u64::from(ValueBits::TAG_SHIFT));
                let tag = self.bin("lshr", &x, &s, "i64");
                let lazy = self.icmp64("eq", &tag, Value::LAZY_TAG);
                self.exit_if(&lazy, 0);
            }
            St::CheckFunc => {
                let p = self.read(0, "i64");
                let c = self.load(&p, Self::off(Layout::OBJECT_CALL), "i64");
                let c = self.untag(&c);
                let none = self.icmp64("eq", &c, 0);
                self.exit_if(&none, 0);
                let tag = self.load(&c, 0, "i8");
                let want = self.line(&format!("llvm.mlir.constant({} : i8) : i8", Layout::FAST_CLOSURE_TAG));
                let other = self.icmp("ne", &tag, &want, "i8");
                self.exit_if(&other, 0);
                let id = self.load(&c, Self::off(Layout::FAST_CLOSURE + Layout::CLOSURE_FUNC), "i32");
                let f = self.hole(0);
                let f = self.line(&format!("llvm.trunc {f} : i64 to i32"));
                let other = self.icmp("ne", &id, &f, "i32");
                self.exit_if(&other, 0);
            }
            St::CheckShape(dict) => {
                let p = self.read(0, "i64");
                let want = self.hole(0);
                let s = self.load(&p, Self::off(Layout::OBJECT_SHAPE), "i64");
                let other = self.icmp("ne", &s, &want, "i64");
                self.exit_if(&other, 0);
                let class = self.load(&p, Self::off(Layout::OBJECT_CLASS), "i8");
                let bit = self.line(&format!("llvm.mlir.constant({} : i8) : i8", Layout::OBJECT_PLAIN_BIT));
                let plain = self.bin("and", &class, &bit, "i8");
                let zero = self.line("llvm.mlir.constant(0 : i8) : i8");
                let exotic = self.icmp("eq", &plain, &zero, "i8");
                self.exit_if(&exotic, 0);
                if dict {
                    let d = self.load(&p, Self::off(Layout::OBJECT_DICT), "i64");
                    let has = self.icmp64("ne", &d, 0);
                    self.exit_if(&has, 0);
                }
            }
            St::Check(expect) => {
                let c = self.read(0, "i64");
                let fail = self.icmp64("eq", &c, u64::from(!expect));
                self.exit_if(&fail, 0);
            }
            St::CmpInt(c) => {
                let (a, b) = (self.read(0, "i32"), self.read(1, "i32"));
                let r = self.icmp(Self::signed(c), &a, &b, "i32");
                self.write_bool(&r);
            }
            St::CmpWord(c) => {
                let (a, b) = (self.read(0, "i64"), self.read(1, "i64"));
                let r = self.icmp(Self::signed(c), &a, &b, "i64");
                self.write_bool(&r);
            }
            St::CmpFloat(c) => {
                let (a, b) = (self.read(0, "f64"), self.read(1, "f64"));
                let p = match c {
                    Cond::Lt => "olt",
                    Cond::Le => "ole",
                    Cond::Gt => "ogt",
                    Cond::Ge => "oge",
                    Cond::Eq => "oeq",
                    Cond::Ne => "one",
                };
                let r = self.line(&format!("llvm.fcmp \"{p}\" {a}, {b} : f64"));
                self.write_bool(&r);
            }
            St::LoadWord(ptr) => {
                let base = self.read(0, "i64");
                let off = self.hole(0);
                let a = self.bin("add", &base, &off, "i64");
                let v = self.load(&a, 0, "i64");
                let v = if ptr { self.untag(&v) } else { v };
                self.write(&v, "i64");
            }
            St::StoreCell => {
                let base = self.read(0, "i64");
                let w = self.read(1, "i64");
                let off = self.hole(0);
                let addr = self.bin("add", &base, &off, "i64");
                let old = self.load(&addr, 0, "i64");
                let any = self.bin("or", &old, &w, "i64");
                let gc = self.icmp64("uge", &any, j8framework::paragc::WORD_POINTER_MIN);
                let (slow, fast, done) = (self.label(), self.label(), self.label());
                self.stmt(&format!("llvm.cond_br {gc}, {slow}, {fast}"));
                let _ = writeln!(self.out, "  {fast}:");
                let p = self.at(&addr, 0);
                self.stmt(&format!("llvm.store {w}, {p} : i64, !llvm.ptr"));
                self.stmt(&format!("llvm.br {done}"));
                let _ = writeln!(self.out, "  {slow}:");
                let id = self.hole(1);
                let z = self.c64(0);
                self.call("j8_helper", &[String::from("%ctx"), id, addr, w, z.clone(), z]);
                self.stmt(&format!("llvm.br {done}"));
                let _ = writeln!(self.out, "  {done}:");
            }
            St::StoreCellAddr => {
                let o = self.read(0, "i64");
                let s = self.load(&o, Self::off(Layout::OBJECT_STORE), "i64");
                let s = self.untag(&s);
                let k = self.hole(0);
                let r = self.bin("add", &s, &k, "i64");
                self.write(&r, "i64");
            }
            St::ContextCell => {
                let c = self.read(0, "i64");
                let boxed = self.load(&c, Self::off(Layout::ENV_BOXED_PTR), "i64");
                let k = self.hole(0);
                let boxed = self.bin("add", &boxed, &k, "i64");
                let ik = self.hole(1);
                let inline = self.bin("add", &c, &ik, "i64");
                let tag = self.load(&c, Self::off(Layout::ENV_TAG), "i8");
                let want = self.line(&format!("llvm.mlir.constant({} : i8) : i8", Layout::ENV_INLINE_TAG));
                let out = self.icmp("ne", &tag, &want, "i8");
                let r = self.line(&format!("llvm.select {out}, {boxed}, {inline} : i1, i64"));
                self.write(&r, "i64");
            }
            St::Poll => {
                let b = self.load("%ctx", Self::field(Ctx::BUDGET), "i64");
                let one = self.c64(1);
                let left = self.bin("sub", &b, &one, "i64");
                let p = self.at("%ctx", Self::field(Ctx::BUDGET));
                self.stmt(&format!("llvm.store {left}, {p} : i64, !llvm.ptr"));
                let zero = self.c64(0);
                let out = self.icmp("sle", &left, &zero, "i64");
                let (slow, done) = (self.label(), self.label());
                self.stmt(&format!("llvm.cond_br {out}, {slow}, {done}"));
                let _ = writeln!(self.out, "  {slow}:");
                let r = self.call("j8_poll", &[String::from("%ctx")]);
                let stop = self.icmp64("ne", &r, 0);
                self.exit_if(&stop, 0);
                self.stmt(&format!("llvm.br {done}"));
                let _ = writeln!(self.out, "  {done}:");
            }
            St::Global => {
                let cache = self.hole(0);
                let epoch = self.load("%ctx", Self::field(Ctx::EPOCH), "i64");
                let seen = self.load(&cache, 0, "i64");
                let hit = self.icmp("eq", &epoch, &seen, "i64");
                let (fast, slow, done) = (self.label(), self.label(), self.label());
                self.stmt(&format!("llvm.cond_br {hit}, {fast}, {slow}"));
                let _ = writeln!(self.out, "  {fast}:");
                let v = self.load(&cache, 8, "i64");
                self.write(&v, "i64");
                self.stmt(&format!("llvm.br {done}"));
                let _ = writeln!(self.out, "  {slow}:");
                let id = self.hole(1);
                let z = self.c64(0);
                let r = self.call("j8_helper", &[String::from("%ctx"), id, cache, z.clone(), z.clone(), z]);
                let miss = self.load("%ctx", Self::field(Ctx::MISS), "i64");
                let missed = self.icmp64("ne", &miss, 0);
                self.exit_if(&missed, 0);
                self.pending(1);
                self.write(&r, "i64");
                self.stmt(&format!("llvm.br {done}"));
                let _ = writeln!(self.out, "  {done}:");
            }
            St::Enter => {
                let sp = self.line("llvm.intr.stacksave : !llvm.ptr");
                let sp = self.line(&format!("llvm.ptrtoint {sp} : !llvm.ptr to i64"));
                let floor = self.load("%ctx", Self::field(Ctx::STACK_FLOOR), "i64");
                let deep = self.icmp("ult", &sp, &floor, "i64");
                self.exit_if(&deep, 0);
            }
            St::Helper(n, throws, result) => {
                let id = self.hole(0);
                let mut args = vec![String::from("%ctx"), id];
                for k in 0..n {
                    args.push(self.read(k, "i64"));
                }
                let z = self.c64(0);
                args.resize(2 + usize::from(HELPER_REGS), z);
                self.helper_tail(&args, throws, result);
            }
            St::HelperMany(throws, result) => {
                let id = self.hole(0);
                let n = self.hole(1);
                let z = self.c64(0);
                let args = vec![String::from("%ctx"), id, n, z.clone(), z.clone(), z];
                self.helper_tail(&args, throws, result);
            }
            St::CallJit(n) => {
                let mut args = vec![String::from("%ctx")];
                for k in 0..2 + n {
                    args.push(self.read(k, "i64"));
                }
                let undef = self.c64(ValueBits::UNDEFINED);
                args.resize(3 + usize::from(JIT_ARGS), undef);
                let r = self.call("call.target", &args);
                self.pending(0);
                self.write(&r, "i64");
            }
        }
    }

    fn helper_tail(&mut self, args: &[String], throws: bool, result: bool) {
        let r = self.call("j8_helper", args);
        if throws {
            self.pending(0);
        }
        if result {
            self.write(&r, "i64");
        }
    }

    fn int(&mut self, op: IntOp, checked: bool) {
        let (a, b) = (self.read(0, "i32"), self.read(1, "i32"));
        let r = match (op, checked) {
            (IntOp::Add | IntOp::Sub | IntOp::Mul, true) => {
                let wa = self.line(&format!("llvm.sext {a} : i32 to i64"));
                let wb = self.line(&format!("llvm.sext {b} : i32 to i64"));
                let name = match op {
                    IntOp::Add => "add",
                    IntOp::Sub => "sub",
                    _ => "mul",
                };
                let w = self.bin(name, &wa, &wb, "i64");
                let r = self.line(&format!("llvm.trunc {w} : i64 to i32"));
                let back = self.line(&format!("llvm.sext {r} : i32 to i64"));
                let over = self.icmp("ne", &back, &w, "i64");
                self.exit_if(&over, 0);
                if op == IntOp::Mul {
                    let sign = self.bin("or", &a, &b, "i32");
                    self.minus_zero(&r, &sign);
                }
                r
            }
            (IntOp::Mod, _) => {
                let zero = self.c32(0);
                let z = self.icmp("eq", &b, &zero, "i32");
                self.exit_if(&z, 0);
                let minus = self.c32(-1);
                let m = self.icmp("eq", &b, &minus, "i32");
                self.exit_if(&m, 0);
                let r = self.bin("srem", &a, &b, "i32");
                self.minus_zero(&r, &a);
                r
            }
            (IntOp::Shl | IntOp::Sar | IntOp::Shr, _) => {
                let mask = self.c32(31);
                let n = self.bin("and", &b, &mask, "i32");
                let name = match op {
                    IntOp::Shl => "shl",
                    IntOp::Sar => "ashr",
                    _ => "lshr",
                };
                let r = self.bin(name, &a, &n, "i32");
                if checked {
                    let zero = self.c32(0);
                    let neg = self.icmp("slt", &r, &zero, "i32");
                    self.exit_if(&neg, 0);
                }
                r
            }
            _ => {
                let name = match op {
                    IntOp::Add => "add",
                    IntOp::Sub => "sub",
                    IntOp::Mul => "mul",
                    IntOp::And => "and",
                    IntOp::Or => "or",
                    _ => "xor",
                };
                self.bin(name, &a, &b, "i32")
            }
        };
        self.write_i32(&r);
    }

    fn minus_zero(&mut self, r: &str, sign: &str) {
        let zero = self.c32(0);
        let z = self.icmp("eq", r, &zero, "i32");
        let neg = self.icmp("slt", sign, &zero, "i32");
        let both = self.bin("and", &z, &neg, "i1");
        self.exit_if(&both, 0);
    }
}

#[derive(Clone, Copy, Debug)]
enum Fill {
    Operand(u8),
    Dest,
    Const(u8),
    Jump(u8),
    Sym(Sym),
    Target,
}

struct Piece {
    code: Vec<u8>,
    holes: Vec<(usize, HoleKind, Fill)>,
}

impl TryFrom<&Stencil> for Piece {
    type Error = Unsupported;

    fn try_from(s: &Stencil) -> Stitching<Piece> {
        let mut holes = Vec::with_capacity(s.holes.len());
        for h in &s.holes {
            let num = |p: &str| h.name.strip_prefix(p).and_then(|k| k.parse::<u8>().ok());
            let fill = if h.name == "slot.d" {
                Fill::Dest
            } else if let Some(k) = num("slot.") {
                Fill::Operand(k)
            } else if let Some(k) = num("hole.") {
                Fill::Const(k)
            } else if let Some(k) = num("jump.") {
                Fill::Jump(k)
            } else if h.name == "call.target" {
                Fill::Target
            } else {
                Fill::Sym(Sym::try_from(h.name.as_str())?)
            };
            holes.push((h.at, h.kind, fill));
        }
        Ok(Piece { code: s.code.clone(), holes })
    }
}

impl TryFrom<&str> for Sym {
    type Error = Unsupported;

    fn try_from(s: &str) -> Stitching<Sym> {
        match s {
            "j8_deopt" => Ok(Sym::Deopt),
            "j8_poll" => Ok(Sym::Poll),
            "j8_helper" => Ok(Sym::Helper),
            s => Err(Unsupported(format!("a stencil calls unknown @{s}"))),
        }
    }
}

pub struct Table {
    pieces: Vec<(St, Piece)>,
}

impl Table {
    pub fn compile(compiler: &crate::Compiler) -> Result<Table, crate::CompileError> {
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
        let stencils: Stencils = compiler.stencils(&text, &emit)?;
        let mut pieces = Vec::with_capacity(all.len());
        for st in all {
            let s = stencils.get(&st.name()).ok_or_else(|| crate::CompileError::Backend(format!("no stencil {}", st.name())))?;
            pieces.push((st, Piece::try_from(s).map_err(|e| crate::CompileError::Backend(e.0))?));
        }
        Ok(Table { pieces })
    }

    fn get(&self, st: St) -> Stitching<&Piece> {
        self.pieces.iter().find(|(s, _)| *s == st).map(|(_, p)| p).ok_or_else(|| Unsupported(format!("no stencil for {st:?}")))
    }
}

struct Stub {
    fs: Fs,
    reason: Reason,
    hint: Option<Fs>,
    label: Label,
    op: V,
}

struct Use<'p> {
    piece: &'p Piece,
    operands: Vec<i32>,
    dest: Option<i32>,
    consts: Vec<u64>,
    jumps: Vec<Label>,
    target: Option<u32>,
}

pub struct Stitch<'g, 't> {
    g: &'g Graph,
    kind: Kind,
    globals: &'g [u64],
    table: &'t Table,
    a: Asm,
    slot: Vec<Option<i32>>,
    temps: i32,
    params: i32,
    regs: i32,
    blocks: Vec<Option<Label>>,
    epilogue: Label,
    jumps: Vec<(usize, Label, HoleKind)>,
    relocs: Vec<(usize, Sym)>,
    stubs: Vec<Stub>,
    exits: Vec<Exit>,
    values: Vec<Vec<(V, u32)>>,
    calls: Vec<Helper>,
    arity: Vec<usize>,
    deopt_base: u32,
    helper_base: u32,
    cur: V,
}

impl<'g, 't> Stitch<'g, 't> {
    const FIRST_SLOT: i32 = crate::live::Template::SPILL_TOP - 8 * crate::live::Template::SPILL_WORDS - 8;

    pub fn stitch(g: &'g Graph, kind: Kind, deopt_base: u32, helper_base: u32, globals: &'g [u64], table: &'t Table) -> Stitching<Generated> {
        if !g.blocks[0].preds.is_empty() {
            return Err(Unsupported("the entry block has predecessors".into()));
        }
        let mut n = 0i32;
        let mut slot = vec![None; g.insts.len()];
        for (v, i) in g.insts.iter().enumerate() {
            if i.rep != Rep::None {
                slot[v] = Some(Self::FIRST_SLOT - 8 * n);
                n += 1;
            }
        }
        let phis = g.blocks.iter().map(|b| b.insts.iter().filter(|&&v| matches!(g.op(v), Op::Phi(_))).count()).max().unwrap_or(0);
        let spare = i32::try_from(phis.max(2 + usize::from(JIT_ARGS))).unwrap_or(i32::MAX);
        let disp = |k: i32| Self::FIRST_SLOT - 8 * k;
        let (params, regs) = (disp(n), disp(n + 1));
        let temps = disp(n + 2 + spare - 1);
        n += 2 + spare + crate::live::Template::SPILL_WORDS;
        let frame = (n * 8 + 15) / 16 * 16 + 8;
        let mut a = Asm::default();
        let epilogue = a.label();
        let mut me = Stitch { g, kind, globals, table, a, slot, temps, params, regs, blocks: vec![None; g.blocks.len()], epilogue, jumps: Vec::new(), relocs: Vec::new(), stubs: Vec::new(), exits: Vec::new(), values: Vec::new(), calls: Vec::new(), arity: Vec::new(), deopt_base, helper_base, cur: 0 };
        me.run(frame)?;
        let _ = me.a.finish();
        for &(at, l, kind) in &me.jumps {
            let to = me.a.at(l).ok_or_else(|| Unsupported("a jump to an unbound label".into()))?;
            if kind == HoleKind::Branch26 {
                Stencil::branch26(&mut me.a.bytes, at, to);
            } else {
                Stencil::rel32(&mut me.a.bytes, at, to);
            }
        }
        Ok(Generated { bytes: me.a.bytes, relocs: me.relocs, exits: me.exits, values: me.values, calls: me.calls, arity: me.arity })
    }

    fn rpo(&self) -> Vec<B> {
        let mut seen = vec![false; self.g.blocks.len()];
        let mut post = Vec::new();
        let mut stack: Vec<(B, usize)> = vec![(0, 0)];
        seen[0] = true;
        while let Some((b, i)) = stack.pop() {
            let succ = self.g.succs(b);
            match succ.get(i) {
                Some(&s) => {
                    stack.push((b, i + 1));
                    if !seen[s as usize] {
                        seen[s as usize] = true;
                        stack.push((s, 0));
                    }
                }
                None => post.push(b),
            }
        }
        post.reverse();
        let (hot, cold): (Vec<B>, Vec<B>) = post.into_iter().partition(|b| !self.g.blocks[*b as usize].cold);
        hot.into_iter().chain(cold).collect()
    }

    fn run(&mut self, frame: i32) -> Stitching<()> {
        let order = self.rpo();
        for &b in &order {
            self.blocks[b as usize] = Some(self.a.label());
        }
        self.a.enter(frame);
        self.a.mov_rr(R15, RDI);
        match self.kind {
            Kind::Native(_) => {
                for &v in &self.g.blocks[0].insts.clone() {
                    let from = match self.g.op(v) {
                        Op::Callee => RSI,
                        Op::Param(i) => [RDX, RCX, R8, R9][(*i as usize).min(3)],
                        _ => continue,
                    };
                    let d = self.dest(v)?;
                    self.a.store(RBP, d, from);
                }
            }
            _ => {
                self.a.store(RBP, self.params, RSI);
                self.a.store(RBP, self.regs, RDX);
            }
        }
        for &b in &order {
            let l = self.blocks[b as usize].expect("a label");
            self.a.bind(l);
            for &v in &self.g.blocks[b as usize].insts.clone() {
                self.cur = v;
                self.inst(b, v)?;
            }
        }
        for i in 0..self.stubs.len() {
            self.stub(i);
        }
        self.a.bind(self.epilogue);
        self.a.leave();
        Ok(())
    }

    fn dest(&self, v: V) -> Stitching<i32> {
        self.slot[v as usize].ok_or_else(|| Unsupported(format!("v{v} has no slot")))
    }

    fn put(&mut self, u: &Use<'_>) {
        let start = self.a.here();
        self.a.bytes.extend_from_slice(&u.piece.code);
        for &(at, kind, fill) in &u.piece.holes {
            let at = start + at;
            match (kind, fill) {
                (HoleKind::Disp32, Fill::Operand(k)) => Stencil::disp32(&mut self.a.bytes, at, u.operands[k as usize]),
                (HoleKind::Disp32, Fill::Dest) => Stencil::disp32(&mut self.a.bytes, at, u.dest.unwrap_or(0)),
                (HoleKind::Abs64, Fill::Const(k)) => Stencil::abs64(&mut self.a.bytes, at, u.consts[k as usize]),
                (HoleKind::Abs64, Fill::Sym(s)) => self.relocs.push((at, s)),
                (HoleKind::Abs64, Fill::Target) => self.relocs.push((at, Sym::Target(u.target.unwrap_or(0)))),
                (HoleKind::Rel32 | HoleKind::Branch26, Fill::Jump(k)) => self.jumps.push((at, u.jumps[k as usize], kind)),
                _ => {}
            }
        }
    }

    fn piece(&self, st: St) -> Stitching<&'t Piece> {
        self.table.get(st)
    }

    fn slots(&mut self, vs: &[V]) -> Stitching<Vec<i32>> {
        vs.iter().map(|&v| self.operand(v)).collect()
    }

    fn operand(&mut self, v: V) -> Stitching<i32> {
        let d = self.dest(v)?;
        if let Some(bits) = self.constant(v) {
            self.a.mov_ri(RAX, bits);
            self.a.store(RBP, d, RAX);
        }
        Ok(d)
    }

    fn constant(&self, v: V) -> Option<u64> {
        let rep = self.g.rep(v);
        match self.g.op(v) {
            Op::I32(x) if rep == Rep::Bool => Some(u64::from(*x != 0)),
            Op::I32(x) => Some(i64::from(*x).cast_unsigned()),
            Op::F64(x) => Some(x.to_bits()),
            Op::Word(w) => Some(*w),
            _ => None,
        }
    }

    fn exit(&mut self, fs: Fs, reason: Reason) -> Label {
        let hint = self.g.hoisted.iter().find(|(v, _)| *v == self.cur).map(|(_, f)| *f);
        if let Some(s) = self.stubs.iter().find(|s| s.fs == fs && s.reason == reason && s.hint == hint) {
            return s.label;
        }
        let label = self.a.label();
        self.stubs.push(Stub { fs, reason, hint, label, op: self.cur });
        label
    }

    fn helper_id(&mut self, h: Helper, n: usize) -> u64 {
        let id = self.helper_base + u32::try_from(self.calls.len()).unwrap_or(0);
        self.calls.push(h);
        self.arity.push(n);
        u64::from(id)
    }

    fn emit(&mut self, st: St, operands: &[V], dest: Option<V>, consts: Vec<u64>, jumps: Vec<Label>) -> Stitching<()> {
        let piece = self.piece(st)?;
        let operands = self.slots(operands)?;
        let dest = match dest {
            Some(v) => Some(self.dest(v)?),
            None => None,
        };
        self.put(&Use { piece, operands, dest, consts, jumps, target: None });
        Ok(())
    }

    fn copy(&mut self, from: i32, to: i32) {
        if from != to {
            self.a.load(RAX, RBP, from);
            self.a.store(RBP, to, RAX);
        }
    }

    fn boxed(&mut self, v: V, temp: i32) -> Stitching<i32> {
        let d = self.operand(v)?;
        Ok(match self.g.rep(v) {
            Rep::I32 => {
                let piece = self.piece(St::IntToFloat)?;
                self.put(&Use { piece, operands: vec![d], dest: Some(temp), consts: Vec::new(), jumps: Vec::new(), target: None });
                temp
            }
            Rep::Bool => {
                let piece = self.piece(St::BoxBool)?;
                self.put(&Use { piece, operands: vec![d], dest: Some(temp), consts: Vec::new(), jumps: Vec::new(), target: None });
                temp
            }
            _ => d,
        })
    }

    fn edge(&mut self, from: B, to: B) -> Stitching<()> {
        let blk = &self.g.blocks[to as usize];
        let Some(k) = blk.preds.iter().position(|&p| p == from) else { return Ok(()) };
        let mut moves = Vec::new();
        for &p in &blk.insts {
            if let Op::Phi(ins) = self.g.op(p) {
                moves.push((ins[k], p));
            }
        }
        let mut pairs = Vec::with_capacity(moves.len());
        for (src, dst) in moves {
            pairs.push((self.operand(src)?, self.dest(dst)?));
        }
        let tangled = pairs.iter().any(|(_, d)| pairs.iter().any(|(s, _)| s == d));
        if tangled {
            for (i, &(s, _)) in pairs.iter().enumerate() {
                self.copy(s, self.temps + 8 * i32::try_from(i).unwrap_or(0));
            }
            for (i, &(_, d)) in pairs.iter().enumerate() {
                self.copy(self.temps + 8 * i32::try_from(i).unwrap_or(0), d);
            }
        } else {
            for (s, d) in pairs {
                self.copy(s, d);
            }
        }
        Ok(())
    }

    fn goto(&mut self, b: B) {
        let l = self.blocks[b as usize].expect("a reachable block");
        self.a.jmp(l);
    }

    fn inst(&mut self, b: B, v: V) -> Stitching<()> {
        let op = self.g.op(v).clone();
        match op {
            Op::Phi(_) | Op::I32(_) | Op::F64(_) | Op::Word(_) => {}
            Op::Callee => {
                if !matches!(self.kind, Kind::Native(_)) {
                    return Err(Unsupported("the callee outside a native entry".into()));
                }
            }
            Op::Param(i) => {
                if !matches!(self.kind, Kind::Native(_)) {
                    self.param(v, i as usize)?;
                }
            }
            Op::CtxField(off) => {
                let d = self.dest(v)?;
                self.a.load(RAX, R15, off);
                self.a.store(RBP, d, RAX);
            }
            Op::Int(op, x, y, fs) => {
                let checked = fs.is_some() && !op.total();
                let jumps = match fs {
                    Some(fs) if checked => vec![self.exit(fs, Reason::Int)],
                    _ => Vec::new(),
                };
                if op == IntOp::Mod && fs.is_none() {
                    return Err(Unsupported("a modulus without a frame state".into()));
                }
                self.emit(St::Int(op, checked), &[x, y], Some(v), Vec::new(), jumps)?;
            }
            Op::Float(op, x, y) => self.emit(St::Float(op), &[x, y], Some(v), Vec::new(), Vec::new())?,
            Op::IntToFloat(x) | Op::BoxInt(x) => self.emit(St::IntToFloat, &[x], Some(v), Vec::new(), Vec::new())?,
            Op::BoxFloat(x) => self.emit(St::BoxFloat, &[x], Some(v), Vec::new(), Vec::new())?,
            Op::BoxBool(x) => self.emit(St::BoxBool, &[x], Some(v), Vec::new(), Vec::new())?,
            Op::BoxPtr(x, tag) => self.emit(St::BoxPtr, &[x], Some(v), vec![tag], Vec::new())?,
            Op::Untag(x) => self.emit(St::Untag, &[x], Some(v), Vec::new(), Vec::new())?,
            Op::UnboxInt(x, fs) => {
                let jumps = vec![self.exit(fs, Reason::Number), self.exit(fs, Reason::Int)];
                self.emit(St::UnboxInt, &[x], Some(v), Vec::new(), jumps)?;
            }
            Op::FloatToInt(x, fs) => {
                let jumps = vec![self.exit(fs, Reason::Int)];
                self.emit(St::FloatToInt, &[x], Some(v), Vec::new(), jumps)?;
            }
            Op::UnboxFloat(x, fs) => {
                let jumps = vec![self.exit(fs, Reason::Number)];
                self.emit(St::UnboxFloat, &[x], Some(v), Vec::new(), jumps)?;
            }
            Op::CheckObject(x, fs) => {
                let jumps = vec![self.exit(fs, Reason::Shape)];
                self.emit(St::CheckObject, &[x], Some(v), Vec::new(), jumps)?;
            }
            Op::CheckNotHole(x, fs) | Op::CheckNotLazy(x, fs) => {
                let (st, reason) = if let Op::CheckNotHole(..) = op { (St::CheckNotHole, Reason::Hole) } else { (St::CheckNotLazy, Reason::Shape) };
                let jumps = vec![self.exit(fs, reason)];
                self.emit(st, &[x], None, Vec::new(), jumps)?;
                if let Some(d) = self.slot[v as usize] {
                    let s = self.operand(x)?;
                    self.copy(s, d);
                }
            }
            Op::CheckFunc(p, f, fs) => {
                let jumps = vec![self.exit(fs, Reason::Func)];
                self.emit(St::CheckFunc, &[p], None, vec![u64::from(f.0)], jumps)?;
            }
            Op::CheckShape(p, shape, fs) => {
                let want = shape.as_ref().map_or(0, |s| alloc::sync::Arc::as_ptr(s) as usize as u64);
                let jumps = vec![self.exit(fs, Reason::Shape)];
                self.emit(St::CheckShape(shape.is_none()), &[p], None, vec![want], jumps)?;
            }
            Op::Check(flag, expect, fs) => {
                let jumps = vec![self.exit(fs, Reason::Shape)];
                self.emit(St::Check(expect), &[flag], None, Vec::new(), jumps)?;
            }
            Op::CmpInt(c, x, y) => {
                let st = if matches!(self.g.rep(x), Rep::I32 | Rep::Bool) { St::CmpInt(c) } else { St::CmpWord(c) };
                self.emit(st, &[x, y], Some(v), Vec::new(), Vec::new())?;
            }
            Op::CmpWord(c, x, y) => self.emit(St::CmpWord(c), &[x, y], Some(v), Vec::new(), Vec::new())?,
            Op::CmpFloat(c, x, y) => self.emit(St::CmpFloat(c), &[x, y], Some(v), Vec::new(), Vec::new())?,
            Op::LoadWord(base, off) | Op::LoadRaw(base, off) => self.emit(St::LoadWord(false), &[base], Some(v), vec![i64::from(off).cast_unsigned()], Vec::new())?,
            Op::LoadPtr(base, off) => self.emit(St::LoadWord(true), &[base], Some(v), vec![i64::from(off).cast_unsigned()], Vec::new())?,
            Op::StoreCell(base, off, w) => {
                let id = self.helper_id(Helper::StoreCell, 2);
                self.emit(St::StoreCell, &[base, w], None, vec![i64::from(off).cast_unsigned(), id], Vec::new())?;
            }
            Op::StoreCellAddr(o, slot) => {
                let k = (Layout::STORE_CELLS + (slot as usize).saturating_sub(Layout::INLINE_PROPS) * Layout::CELL) as u64;
                self.emit(St::StoreCellAddr, &[o], Some(v), vec![k], Vec::new())?;
            }
            Op::ContextCell(c, idx) => {
                let k = idx as usize * Layout::CELL;
                self.emit(St::ContextCell, &[c], Some(v), vec![k as u64, (Layout::ENV_INLINE_CELLS + k) as u64], Vec::new())?;
            }
            Op::Poll(fs) => {
                let jumps = vec![self.exit(fs, Reason::Poll)];
                self.emit(St::Poll, &[], None, Vec::new(), jumps)?;
            }
            Op::Global(k, _, fs) => {
                let id = self.helper_id(Helper::GlobalProbe(k), 1);
                let jumps = vec![self.exit(fs, Reason::Func), self.exit(fs, Reason::Throw)];
                self.emit(St::Global, &[], Some(v), vec![self.globals[k as usize], id], jumps)?;
            }
            Op::Enter(fs) => {
                let jumps = vec![self.exit(fs, Reason::Depth)];
                self.emit(St::Enter, &[], None, Vec::new(), jumps)?;
            }
            Op::Call(h, args, fs) => self.helper(v, h, &args, fs)?,
            Op::CallJit(k, callee, this, args, fs) => {
                let n = u8::try_from(args.len()).ok().filter(|n| *n <= JIT_ARGS).ok_or_else(|| Unsupported("a call with too many arguments".into()))?;
                let mut operands = Vec::with_capacity(2 + args.len());
                for (i, x) in [callee, this].into_iter().chain(args.iter().copied()).enumerate() {
                    let temp = self.temps + 8 * i32::try_from(i).unwrap_or(0);
                    operands.push(self.boxed(x, temp)?);
                }
                let jumps = vec![self.exit(fs, Reason::Throw)];
                let piece = self.piece(St::CallJit(n))?;
                let dest = Some(self.dest(v)?);
                self.put(&Use { piece, operands, dest, consts: Vec::new(), jumps, target: Some(k) });
            }
            Op::Jump(t) => {
                self.edge(b, t)?;
                self.goto(t);
            }
            Op::Branch(c, t, f) => {
                let s = self.operand(c)?;
                let (lt, lf) = (self.a.label(), self.a.label());
                self.a.cmp_mi(true, RBP, s, 0);
                self.a.jcc(crate::asm::Cc::Ne, lt);
                self.a.jmp(lf);
                self.a.bind(lt);
                self.edge(b, t)?;
                self.goto(t);
                self.a.bind(lf);
                self.edge(b, f)?;
                self.goto(f);
            }
            Op::Return(w, pc) => self.ret(w, pc)?,
            Op::Deopt(fs) => {
                let l = self.exit(fs, Reason::Always);
                self.a.jmp(l);
            }
        }
        Ok(())
    }

    fn param(&mut self, v: V, i: usize) -> Stitching<()> {
        let d = self.dest(v)?;
        let (p, r) = (self.g.entry_params, self.g.entry_regs);
        let below = |s: usize| -8 * i32::try_from(s).unwrap_or(0);
        let at = |k: usize| 8 * i32::try_from(k).unwrap_or(0);
        let (base, off) = if i < p {
            (Some(self.params), at(i))
        } else if i < p + r {
            (Some(self.regs), at(i - p))
        } else {
            match i - p - r {
                0 => (Some(self.regs), below(FrameSlots::CONTEXT)),
                1 => (Some(self.regs), below(FrameSlots::CLOSURE)),
                2 => (None, Ctx::ACC),
                3 => (Some(self.regs), below(FrameSlots::NEW_TARGET)),
                _ => (Some(self.regs), below(FrameSlots::NEW_THIS)),
            }
        };
        match base {
            Some(slot) => {
                self.a.load(RCX, RBP, slot);
                self.a.load(RAX, RCX, off);
            }
            None => self.a.load(RAX, R15, off),
        }
        self.a.store(RBP, d, RAX);
        Ok(())
    }

    fn helper(&mut self, v: V, h: Helper, args: &[V], fs: Option<Fs>) -> Stitching<()> {
        let id = self.helper_id(h, args.len());
        let throws = fs.is_some() && h.throws() && h != Helper::ModF;
        let result = self.g.rep(v) != Rep::None;
        let jumps = match fs {
            Some(fs) if throws => vec![self.exit(fs, Reason::Throw)],
            _ => Vec::new(),
        };
        let dest = if result { Some(self.dest(v)?) } else { None };
        let n = args.len();
        if n <= usize::from(HELPER_REGS) {
            let operands = self.slots(args)?;
            let piece = self.piece(St::Helper(u8::try_from(n).unwrap_or(0), throws, result))?;
            self.put(&Use { piece, operands, dest, consts: vec![id], jumps, target: None });
        } else {
            for (i, &x) in args.iter().enumerate() {
                let s = self.operand(x)?;
                self.a.load(RAX, RBP, s);
                self.a.store(R15, Ctx::ARGS + 8 * i32::try_from(i).unwrap_or(0), RAX);
            }
            let piece = self.piece(St::HelperMany(throws, result))?;
            self.put(&Use { piece, operands: Vec::new(), dest, consts: vec![id, n as u64], jumps, target: None });
        }
        Ok(())
    }

    fn ret(&mut self, w: V, pc: u32) -> Stitching<()> {
        let s = self.operand(w)?;
        match self.kind {
            Kind::Native(_) => {
                match *self.g.op(w) {
                    Op::BoxInt(i) => {
                        let si = self.operand(i)?;
                        self.a.load32(RDX, RBP, si);
                        self.a.mov_ri(RCX, Ret::INT);
                        self.a.alu_rr(crate::asm::Alu::Or, true, RDX, RCX);
                    }
                    _ => self.a.mov_ri(RDX, 0),
                }
                self.a.load(RAX, RBP, s);
            }
            _ => {
                self.a.load(RAX, RBP, s);
                self.a.store(R15, Ctx::RET, RAX);
                self.a.mov_ri(RAX, u64::from(pc));
                self.a.store(R15, Ctx::RET_PC, RAX);
                self.a.mov_ri(RAX, RETURNED.cast_unsigned());
            }
        }
        self.a.jmp(self.epilogue);
        Ok(())
    }

    fn state_values(&self, fs: Fs, out: &mut Vec<V>) {
        let st = &self.g.states[fs as usize];
        let add = |s: &Slot, out: &mut Vec<V>| {
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

    fn stub(&mut self, i: usize) {
        let Stub { fs, reason, hint, label, op } = self.stubs[i];
        let id = u32::try_from(self.exits.len()).unwrap_or(0);
        self.exits.push(Exit { fs, reason, op, hint });
        self.a.bind(label);
        let mut vals = Vec::new();
        self.state_values(fs, &mut vals);
        let mut map: Vec<(V, u32)> = Vec::new();
        for v in vals {
            if map.iter().any(|(x, _)| *x == v) || self.constant(v).is_some() {
                continue;
            }
            let Some(s) = self.slot[v as usize] else { continue };
            let k = u32::try_from(map.len()).unwrap_or(0);
            self.a.load(RAX, RBP, s);
            self.a.store(R15, Ctx::BUF + 8 * i32::try_from(k).unwrap_or(0), RAX);
            map.push((v, k));
        }
        self.values.push(map);
        self.a.mov_rr(RDI, R15);
        self.a.mov_ri(RSI, u64::from(self.deopt_base + id));
        let at = self.a.call_abs(0);
        self.relocs.push((at, Sym::Deopt));
        self.a.jmp(self.epilogue);
    }
}
