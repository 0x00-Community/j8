//! Graph ⇒ MLIR (`llvm` dialect), the text mlirpass parses and the x86-64 backend emits.
//!
//! ```text
//! interpreter entry   @main(%ctx, %params, %r0) -> i64     status: RETURNED (value in Ctx::RET) / RESUMED / THREW
//! native entry        @main(%ctx, %callee, %this, %a0, %a1, %a2) -> i64      the value (Ctx::PENDING: it threw)
//! a phi               a block argument
//! a guard             cond_br to a cold block: the frame state's values into Ctx::BUF, then @j8_deopt(%ctx, id)
//! a runtime call      @j8_helper(%ctx, id, a, b, c, d) -> i64 (more operands through Ctx::ARGS)
//! a call to compiled  @t<k>(%ctx, %callee, %this, %a0, %a1, %a2) -> i64 (k: the compilation's target)
//! ```
use alloc::format;
use alloc::string::String;
use alloc::vec::Vec;
use core::fmt::Write;

use j8executor::FrameSlots;
use j8framework::{Layout, ValueBits};

use crate::build::{Kind, OBJECT_TAG};
use crate::ir::{Cond, FloatOp, Graph, Helper, IntOp, Op, Rep, Slot, B, Fs, V};
use crate::runtime::Ctx;

/// What a guard checks, for the hints a failure leaves.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Reason {
    /// Not an int32 / overflowed / −0
    Int,
    /// Not a number
    Number,
    /// Another object / shape / function than expected
    Shape,
    Func,
    /// The hole
    Hole,
    /// The budget ran out
    Poll,
    /// Something thrown
    Throw,
    /// Unconditional (bytecode not compiled)
    Always,
    /// Too deep
    Depth,
}

/// One guard's hand-over: its frame state, why, and whether it throws.
pub struct Exit {
    pub fs: Fs,
    pub reason: Reason,
    /// The instruction that jumps here (the first one, when shared)
    pub op: u32,
    /// A guard moved out of a loop: the frame state it had (a failure's hint goes to its instruction)
    pub hint: Option<Fs>,
}

/// A runtime call's fixed part (`j8_helper`'s id indexes these).
#[derive(Clone, Debug)]
pub enum Call {
    Helper(Helper),
}

/// The lowered text and what the runtime needs to read it.
pub struct Lowered {
    pub text: String,
    /// By local deopt id (the global id is `deopt_base +` it)
    pub exits: Vec<Exit>,
    /// Where each frame-state value went in Ctx::BUF, by local deopt id: (value, buffer index)
    pub values: Vec<Vec<(V, u32)>>,
    /// By local helper id
    pub calls: Vec<Helper>,
    /// Operand counts of `calls`
    pub arity: Vec<usize>,
}

/// Status codes of an interpreter entry
pub const RETURNED: i64 = 0;
pub const RESUMED: i64 = 1;
pub const THREW: i64 = 2;

/// Lowers `g` (compiled as `kind`); ids for deopts and helpers start at the given bases.
pub struct Lower<'g> {
    g: &'g Graph,
    kind: Kind,
    out: String,
    cold: String,
    tmp: u32,
    deopt_base: u32,
    helper_base: u32,
    exits: Vec<Exit>,
    values: Vec<Vec<(V, u32)>>,
    calls: Vec<Helper>,
    arity: Vec<usize>,
    targets: usize,
}

fn ty(r: Rep) -> &'static str {
    match r {
        Rep::I32 => "i32",
        Rep::F64 => "f64",
        Rep::Bool => "i1",
        _ => "i64",
    }
}

impl<'g> Lower<'g> {
    pub fn lower(g: &'g Graph, kind: Kind, targets: usize, deopt_base: u32, helper_base: u32) -> Lowered {
        let mut l = Lower { g, kind, out: String::new(), cold: String::new(), tmp: 0, deopt_base, helper_base, exits: Vec::new(), values: Vec::new(), calls: Vec::new(), arity: Vec::new(), targets };
        l.module();
        Lowered { text: l.out, exits: l.exits, values: l.values, calls: l.calls, arity: l.arity }
    }

    fn t(&mut self) -> String {
        self.tmp += 1;
        format!("%t{}", self.tmp)
    }

    fn v(v: V) -> String {
        format!("%v{v}")
    }

    /// A line of the current function body
    fn line(&mut self, s: &str) {
        self.out.push_str("    ");
        self.out.push_str(s);
        self.out.push('\n');
    }

    fn cline(&mut self, s: &str) {
        self.cold.push_str("    ");
        self.cold.push_str(s);
        self.cold.push('\n');
    }

    /// A 64-bit constant
    fn k64(&mut self, x: i64) -> String {
        let t = self.t();
        self.line(&format!("{t} = llvm.mlir.constant({x} : i64) : i64"));
        t
    }

    fn k32(&mut self, x: i32) -> String {
        let t = self.t();
        self.line(&format!("{t} = llvm.mlir.constant({x} : i32) : i32"));
        t
    }

    #[expect(clippy::cast_possible_wrap, reason = "words are bit patterns")]
    fn kw(&mut self, w: u64) -> String {
        self.k64(w as i64)
    }

    /// `base + off` as a pointer
    fn addr(&mut self, base: &str, off: i64) -> String {
        let p = self.t();
        self.line(&format!("{p} = llvm.inttoptr {base} : i64 to !llvm.ptr"));
        if off == 0 {
            return p;
        }
        let q = self.t();
        self.line(&format!("{q} = llvm.getelementptr {p}[{off}] : (!llvm.ptr) -> !llvm.ptr, i8"));
        q
    }

    fn load(&mut self, base: &str, off: i64, t: &str) -> String {
        let p = self.addr(base, off);
        let r = self.t();
        self.line(&format!("{r} = llvm.load {p} : !llvm.ptr -> {t}"));
        r
    }

    fn store(&mut self, base: &str, off: i64, v: &str, t: &str) {
        let p = self.addr(base, off);
        self.line(&format!("llvm.store {v}, {p} : {t}, !llvm.ptr"));
    }

    fn bin(&mut self, op: &str, a: &str, b: &str, t: &str) -> String {
        let r = self.t();
        self.line(&format!("{r} = llvm.{op} {a}, {b} : {t}"));
        r
    }

    fn icmp(&mut self, pred: &str, a: &str, b: &str, t: &str) -> String {
        let r = self.t();
        self.line(&format!("{r} = llvm.icmp \"{pred}\" {a}, {b} : {t}"));
        r
    }

    fn cast(&mut self, op: &str, a: &str, from: &str, to: &str) -> String {
        let r = self.t();
        self.line(&format!("{r} = llvm.{op} {a} : {from} to {to}"));
        r
    }

    fn module(&mut self) {
        let mut m = String::from("module {\n");
        m.push_str("  llvm.func @j8_deopt(i64, i64) -> i64\n");
        m.push_str("  llvm.func @j8_poll(i64) -> i64\n");
        m.push_str("  llvm.func @j8_helper(i64, i64, i64, i64, i64, i64) -> i64\n");
        for k in 0..self.targets {
            let _ = writeln!(m, "  llvm.func @t{k}(i64, i64, i64, i64, i64, i64) -> i64");
        }
        let sig = match self.kind {
            Kind::Native(_) => "%ctx: i64, %callee: i64, %this: i64, %a0: i64, %a1: i64, %a2: i64",
            _ => "%ctx: i64, %params: i64, %r0: i64",
        };
        let _ = writeln!(m, "  llvm.func @main({sig}) -> i64 {{");
        self.out = m;
        self.body();
        let cold = core::mem::take(&mut self.cold);
        self.out.push_str(&cold);
        self.out.push_str("  }\n}\n");
    }

    /// Blocks reachable from the entry, in reverse postorder
    fn order(&self) -> Vec<B> {
        let n = self.g.blocks.len();
        let mut seen = alloc::vec![false; n];
        let mut post = Vec::new();
        let mut stack: Vec<(B, usize)> = alloc::vec![(0, 0)];
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
        post
    }

    fn body(&mut self) {
        let order = self.order();
        for (i, &b) in order.iter().enumerate() {
            let blk = &self.g.blocks[b as usize];
            let phis: Vec<V> = blk.insts.iter().copied().filter(|&v| matches!(self.g.op(v), Op::Phi(_))).collect();
            if i > 0 {
                let args: Vec<String> = phis.iter().map(|&p| format!("{}: {}", Self::v(p), ty(self.g.rep(p)))).collect();
                if args.is_empty() {
                    self.out.push_str(&format!("  ^b{b}:\n"));
                } else {
                    self.out.push_str(&format!("  ^b{b}({}):\n", args.join(", ")));
                }
            } else {
                self.entry();
            }
            let insts: Vec<V> = blk.insts.clone();
            for v in insts {
                if matches!(self.g.op(v), Op::Phi(_)) {
                    continue;
                }
                self.inst(b, v);
            }
        }
    }

    /// The entry block needs nothing set up (natives count their depth in `Op::Enter`)
    fn entry(&mut self) {}

    /// The value of IR value `v` as a word (for frame states and boxing): i64 bits by representation
    fn bits64(&mut self, v: V) -> String {
        let s = Self::v(v);
        match self.g.rep(v) {
            Rep::I32 => self.cast("sext", &s, "i32", "i64"),
            Rep::F64 => self.cast("bitcast", &s, "f64", "i64"),
            Rep::Bool => self.cast("zext", &s, "i1", "i64"),
            _ => s,
        }
    }

    /// A cold block handing over with frame state `fs`; gives its label. Values go into the buffer first.
    fn exit(&mut self, fs: Fs, reason: Reason) -> String {
        let id = u32::try_from(self.exits.len()).unwrap_or(0);
        self.exits.push(Exit { fs, reason, op: 0, hint: None });
        let label = format!("^d{id}");
        let st = &self.g.states[fs as usize];
        let mut vals: Vec<V> = Vec::new();
        let mut add = |s: &Slot, vals: &mut Vec<V>| {
            if let Slot::Value(v) = s
                && !vals.contains(v)
            {
                vals.push(*v);
            }
        };
        for fr in &st.frames {
            add(&fr.callee, &mut vals);
            fr.params.iter().for_each(|s| add(s, &mut vals));
            fr.regs.iter().for_each(|s| add(s, &mut vals));
            add(&fr.context, &mut vals);
            add(&fr.acc, &mut vals);
            if let Some((a, b)) = &fr.construct {
                add(a, &mut vals);
                add(b, &mut vals);
            }
        }
        for o in &st.objects {
            if !vals.contains(&o.proto) {
                vals.push(o.proto);
            }
            o.fields.iter().for_each(|s| add(s, &mut vals));
        }
        for (c, _, s) in &st.writes {
            if !vals.contains(c) {
                vals.push(*c);
            }
            add(s, &mut vals);
        }
        // Constants are known to the runtime: only the others go through the buffer
        let mut map = Vec::new();
        let saved = core::mem::take(&mut self.out);
        self.out.push_str(&format!("  {label}:\n"));
        let mut slot = 0u32;
        for v in vals {
            if matches!(self.g.op(v), Op::I32(_) | Op::F64(_) | Op::Word(_)) {
                continue;
            }
            let w = self.bits64(v);
            self.store("%ctx", i64::from(Ctx::BUF) + 8 * i64::from(slot), &w, "i64");
            map.push((v, slot));
            slot += 1;
        }
        let k = self.k64(i64::from(self.deopt_base + id));
        let r = self.t();
        self.line(&format!("{r} = llvm.call @j8_deopt(%ctx, {k}) : (i64, i64) -> i64"));
        self.ret_raw(&r);
        let block = core::mem::replace(&mut self.out, saved);
        self.cold.push_str(&block);
        self.values.push(map);
        label
    }

    /// Returns `r` from the function (natives count their depth back down)
    fn ret_raw(&mut self, r: &str) {
        if let Kind::Native(_) = self.kind {
            let d = self.load("%ctx", i64::from(Ctx::DEPTH), "i64");
            let one = self.k64(1);
            let d1 = self.bin("sub", &d, &one, "i64");
            self.store("%ctx", i64::from(Ctx::DEPTH), &d1, "i64");
        }
        self.line(&format!("llvm.return {r} : i64"));
    }

    /// Continues only when `ok` holds; otherwise hands over with `fs`
    fn guard(&mut self, ok: &str, fs: Fs, reason: Reason) {
        let d = self.exit(fs, reason);
        self.tmp += 1;
        let cont = format!("^c{}", self.tmp);
        self.line(&format!("llvm.cond_br {ok}, {cont}, {d}"));
        self.out.push_str(&format!("  {cont}:\n"));
    }

    /// After a call that may throw: hands over (throwing) when Ctx::PENDING is set
    fn check_pending(&mut self, fs: Fs) {
        let p = self.load("%ctx", i64::from(Ctx::PENDING), "i64");
        let z = self.k64(0);
        let ok = self.icmp("eq", &p, &z, "i64");
        self.guard(&ok, fs, Reason::Throw);
    }

    /// A runtime call through `j8_helper`
    fn helper(&mut self, h: Helper, args: &[String]) -> String {
        let id = self.helper_base + u32::try_from(self.calls.len()).unwrap_or(0);
        self.calls.push(h);
        self.arity.push(args.len());
        let mut a: Vec<String> = Vec::new();
        if args.len() > 4 {
            for (i, x) in args.iter().enumerate() {
                self.store("%ctx", i64::from(Ctx::ARGS) + 8 * i64::try_from(i).unwrap_or(0), x, "i64");
            }
            let n = self.k64(i64::try_from(args.len()).unwrap_or(0));
            a.push(n);
        } else {
            a.extend(args.iter().cloned());
        }
        while a.len() < 4 {
            let z = self.k64(0);
            a.push(z);
        }
        let k = self.k64(i64::from(id));
        let r = self.t();
        self.line(&format!("{r} = llvm.call @j8_helper(%ctx, {k}, {}, {}, {}, {}) : (i64, i64, i64, i64, i64, i64) -> i64", a[0], a[1], a[2], a[3]));
        r
    }

    fn inst(&mut self, b: B, v: V) {
        let op = self.g.op(v).clone();
        let rep = self.g.rep(v);
        let d = Self::v(v);
        match op {
            Op::Phi(_) => {}
            Op::I32(x) if rep == Rep::Bool => self.line(&format!("{d} = llvm.mlir.constant({} : i1) : i1", u8::from(x != 0))),
            Op::I32(x) => self.line(&format!("{d} = llvm.mlir.constant({x} : i32) : i32")),
            Op::F64(x) => {
                #[expect(clippy::cast_possible_wrap, reason = "bits")]
                let k = self.k64(x.to_bits() as i64);
                self.line(&format!("{d} = llvm.bitcast {k} : i64 to f64"));
            }
            Op::Word(w) => {
                if rep == Rep::Bool {
                    self.line(&format!("{d} = llvm.mlir.constant({} : i1) : i1", u8::from(w != 0)));
                } else {
                    #[expect(clippy::cast_possible_wrap, reason = "bits")]
                    let x = w as i64;
                    self.line(&format!("{d} = llvm.mlir.constant({x} : i64) : i64"));
                }
            }
            Op::Param(i) => self.param(&d, i),
            Op::Callee => {
                let z = self.zero();
                self.line(&format!("{d} = llvm.add %callee, {z} : i64"));
            }
            Op::Enter(fs) => {
                let dep = self.load("%ctx", i64::from(Ctx::DEPTH), "i64");
                let one = self.k64(1);
                let d1 = self.bin("add", &dep, &one, "i64");
                self.store("%ctx", i64::from(Ctx::DEPTH), &d1, "i64");
                let lim = self.load("%ctx", i64::from(Ctx::LIMIT), "i64");
                let ok = self.icmp("ult", &d1, &lim, "i64");
                self.guard(&ok, fs, Reason::Depth);
            }
            Op::Int(op, a, bb, fs) => self.int(&d, op, a, bb, fs),
            Op::Float(op, a, bb) => {
                let name = match op {
                    FloatOp::Add => "fadd",
                    FloatOp::Sub => "fsub",
                    FloatOp::Mul => "fmul",
                    FloatOp::Div => "fdiv",
                };
                self.line(&format!("{d} = llvm.{name} {}, {} : f64", Self::v(a), Self::v(bb)));
            }
            Op::IntToFloat(a) => self.line(&format!("{d} = llvm.sitofp {} : i32 to f64", Self::v(a))),
            Op::BoxInt(a) => {
                let f = self.cast("sitofp", &Self::v(a), "i32", "f64");
                self.line(&format!("{d} = llvm.bitcast {f} : f64 to i64"));
            }
            Op::BoxFloat(a) => {
                let s = Self::v(a);
                let bits = self.cast("bitcast", &s, "f64", "i64");
                let nan = self.t();
                self.line(&format!("{nan} = llvm.fcmp \"uno\" {s}, {s} : f64"));
                let canon = self.kw(ValueBits::CANONICAL_NAN);
                self.line(&format!("{d} = llvm.select {nan}, {canon}, {bits} : i1, i64"));
            }
            Op::BoxBool(a) => {
                let z = self.cast("zext", &Self::v(a), "i1", "i64");
                let f = self.kw(ValueBits::FALSE);
                self.line(&format!("{d} = llvm.add {f}, {z} : i64"));
            }
            Op::BoxPtr(a, tag) => {
                let t = self.kw(tag);
                self.line(&format!("{d} = llvm.or {}, {t} : i64", Self::v(a)));
            }
            Op::Untag(a) => {
                let m = self.kw(ValueBits::PAYLOAD);
                self.line(&format!("{d} = llvm.and {}, {m} : i64", Self::v(a)));
            }
            Op::UnboxInt(a, fs) => {
                let s = Self::v(a);
                let min = self.kw(ValueBits::BOXED_MIN);
                let num = self.icmp("ult", &s, &min, "i64");
                self.guard(&num, fs, Reason::Number);
                let f = self.cast("bitcast", &s, "i64", "f64");
                self.float_to_int(&d, &f, &s, fs);
            }
            Op::FloatToInt(a, fs) => {
                let s = Self::v(a);
                let bits = self.cast("bitcast", &s, "f64", "i64");
                self.float_to_int(&d, &s, &bits, fs);
            }
            Op::UnboxFloat(a, fs) => {
                let s = Self::v(a);
                let min = self.kw(ValueBits::BOXED_MIN);
                let num = self.icmp("ult", &s, &min, "i64");
                self.guard(&num, fs, Reason::Number);
                self.line(&format!("{d} = llvm.bitcast {s} : i64 to f64"));
            }
            Op::CheckObject(a, fs) => {
                let s = Self::v(a);
                let sh = self.k64(48);
                let hi = self.bin("lshr", &s, &sh, "i64");
                let want = self.kw(OBJECT_TAG >> 48);
                let ok = self.icmp("eq", &hi, &want, "i64");
                self.guard(&ok, fs, Reason::Shape);
                let m = self.kw(ValueBits::PAYLOAD);
                self.line(&format!("{d} = llvm.and {s}, {m} : i64"));
            }
            Op::CheckNotHole(a, fs) => {
                let h = self.kw(ValueBits::UNINIT);
                let ok = self.icmp("ne", &Self::v(a), &h, "i64");
                self.guard(&ok, fs, Reason::Hole);
            }
            Op::CheckFunc(p, f, fs) => {
                let s = Self::v(p);
                let fast = self.load(&s, Self::o(Layout::OBJECT_CALL), "i64");
                let m = self.kw(ValueBits::PAYLOAD);
                let fp = self.bin("and", &fast, &m, "i64");
                let z = self.k64(0);
                let ok = self.icmp("ne", &fp, &z, "i64");
                self.guard(&ok, fs, Reason::Func);
                let tag = self.load(&fp, 0, "i8");
                let tz = self.t();
                self.line(&format!("{tz} = llvm.mlir.constant({} : i8) : i8", Layout::FAST_CLOSURE_TAG));
                let ok = self.icmp("eq", &tag, &tz, "i8");
                self.guard(&ok, fs, Reason::Func);
                let id = self.load(&fp, Self::o(Layout::FAST_CLOSURE + Layout::CLOSURE_FUNC), "i32");
                #[expect(clippy::cast_possible_wrap, reason = "an id")]
                let want = self.k32(f.0 as i32);
                let ok = self.icmp("eq", &id, &want, "i32");
                self.guard(&ok, fs, Reason::Func);
            }
            Op::CheckShape(p, shape, fs) => {
                let s = Self::v(p);
                let have = self.load(&s, Self::o(Layout::OBJECT_SHAPE), "i64");
                #[expect(clippy::cast_possible_wrap, reason = "an address")]
                let want = self.k64(shape.as_ref().map_or(0, |x| alloc::sync::Arc::as_ptr(x) as usize as i64));
                let ok = self.icmp("eq", &have, &want, "i64");
                self.guard(&ok, fs, Reason::Shape);
                let class = self.load(&s, Self::o(Layout::OBJECT_CLASS), "i8");
                let bit = self.t();
                self.line(&format!("{bit} = llvm.mlir.constant({} : i8) : i8", Layout::OBJECT_PLAIN_BIT));
                let and = self.bin("and", &class, &bit, "i8");
                let z = self.t();
                self.line(&format!("{z} = llvm.mlir.constant(0 : i8) : i8"));
                let ok = self.icmp("ne", &and, &z, "i8");
                self.guard(&ok, fs, Reason::Shape);
                if shape.is_none() {
                    let dict = self.load(&s, Self::o(Layout::OBJECT_DICT), "i64");
                    let z = self.k64(0);
                    let ok = self.icmp("eq", &dict, &z, "i64");
                    self.guard(&ok, fs, Reason::Shape);
                }
            }
            Op::Check(f, expect, fs) => {
                let s = Self::v(f);
                if expect {
                    self.guard(&s, fs, Reason::Shape);
                } else {
                    let t = self.t();
                    self.line(&format!("{t} = llvm.mlir.constant(true) : i1"));
                    let n = self.bin("xor", &s, &t, "i1");
                    self.guard(&n, fs, Reason::Shape);
                }
            }
            Op::CmpInt(c, a, bb) => self.line(&format!("{d} = llvm.icmp \"{}\" {}, {} : {}", Self::ipred(c), Self::v(a), Self::v(bb), ty(self.g.rep(a)))),
            Op::CmpFloat(c, a, bb) => self.line(&format!("{d} = llvm.fcmp \"{}\" {}, {} : f64", Self::fpred(c), Self::v(a), Self::v(bb))),
            Op::CmpWord(c, a, bb) => self.line(&format!("{d} = llvm.icmp \"{}\" {}, {} : i64", Self::ipred(c), Self::v(a), Self::v(bb))),
            Op::LoadWord(base, off) | Op::LoadRaw(base, off) => {
                let p = self.addr(&Self::v(base), i64::from(off));
                self.line(&format!("{d} = llvm.load {p} : !llvm.ptr -> i64"));
            }
            Op::LoadPtr(base, off) => {
                let w = self.load(&Self::v(base), i64::from(off), "i64");
                let m = self.kw(ValueBits::PAYLOAD);
                self.line(&format!("{d} = llvm.and {w}, {m} : i64"));
            }
            Op::LoadByte(base, off) => {
                let w = self.load(&Self::v(base), i64::from(off), "i8");
                self.line(&format!("{d} = llvm.zext {w} : i8 to i64"));
            }
            Op::StoreCell(base, off, w) => self.store_cell(&Self::v(base), i64::from(off), &Self::v(w)),
            Op::StoreCellAddr(o, slot) => {
                let st = self.load(&Self::v(o), Self::o(Layout::OBJECT_STORE), "i64");
                let m = self.kw(ValueBits::PAYLOAD);
                let p = self.bin("and", &st, &m, "i64");
                let off = Self::o(Layout::STORE_CELLS + (slot as usize - Layout::INLINE_PROPS) * Layout::CELL);
                let k = self.k64(off);
                self.line(&format!("{d} = llvm.add {p}, {k} : i64"));
            }
            Op::ContextCell(c, idx) => {
                let s = Self::v(c);
                let tag = self.load(&s, Self::o(Layout::ENV_TAG), "i8");
                let z = self.t();
                self.line(&format!("{z} = llvm.mlir.constant({} : i8) : i8", Layout::ENV_INLINE_TAG));
                let inline = self.icmp("eq", &tag, &z, "i8");
                let k = self.k64(Self::o(Layout::ENV_INLINE_CELLS + idx as usize * Layout::CELL));
                let a = self.bin("add", &s, &k, "i64");
                let boxed = self.load(&s, Self::o(Layout::ENV_BOXED_PTR), "i64");
                let k2 = self.k64(Self::o(idx as usize * Layout::CELL));
                let b2 = self.bin("add", &boxed, &k2, "i64");
                self.line(&format!("{d} = llvm.select {inline}, {a}, {b2} : i1, i64"));
            }
            Op::CtxField(off) => {
                let p = self.addr("%ctx", i64::from(off));
                self.line(&format!("{d} = llvm.load {p} : !llvm.ptr -> i64"));
            }
            Op::Poll(fs) => {
                let n = self.load("%ctx", i64::from(Ctx::BUDGET), "i64");
                let one = self.k64(1);
                let n1 = self.bin("sub", &n, &one, "i64");
                self.store("%ctx", i64::from(Ctx::BUDGET), &n1, "i64");
                let z = self.k64(0);
                let ok = self.icmp("sgt", &n1, &z, "i64");
                self.tmp += 1;
                let (slow, cont) = (format!("^s{}", self.tmp), format!("^c{}", self.tmp));
                self.line(&format!("llvm.cond_br {ok}, {cont}, {slow}"));
                self.out.push_str(&format!("  {slow}:\n"));
                let r = self.t();
                self.line(&format!("{r} = llvm.call @j8_poll(%ctx) : (i64) -> i64"));
                let z = self.k64(0);
                let go = self.icmp("eq", &r, &z, "i64");
                let ex = self.exit(fs, Reason::Poll);
                self.line(&format!("llvm.cond_br {go}, {cont}, {ex}"));
                self.out.push_str(&format!("  {cont}:\n"));
            }
            Op::Call(h, args, fs) => {
                let ins: Vec<String> = args.iter().map(|a| self.bits64(*a)).collect();
                let r = self.helper(h, &ins);
                if let Some(fs) = fs
                    && h.throws()
                {
                    self.check_pending(fs);
                }
                match h.rep() {
                    Rep::None => {}
                    Rep::F64 => self.line(&format!("{d} = llvm.bitcast {r} : i64 to f64")),
                    Rep::Bool => self.line(&format!("{d} = llvm.trunc {r} : i64 to i1")),
                    _ => {
                        let z = self.zero();
                        self.line(&format!("{d} = llvm.add {r}, {z} : i64"));
                    }
                }
            }
            Op::CallJit(k, callee, this, args, fs) => {
                let mut a: Vec<String> = args.iter().map(|x| Self::v(*x)).collect();
                while a.len() < 3 {
                    let u = self.kw(ValueBits::UNDEFINED);
                    a.push(u);
                }
                self.line(&format!("{d} = llvm.call @t{k}(%ctx, {}, {}, {}, {}, {}) : (i64, i64, i64, i64, i64, i64) -> i64", Self::v(callee), Self::v(this), a[0], a[1], a[2]));
                self.check_pending(fs);
            }
            Op::Jump(t) => self.jump(b, t),
            Op::Branch(c, t, f) => {
                let ta = self.block_args(b, t);
                let fa = self.block_args(b, f);
                self.line(&format!("llvm.cond_br {}, ^b{t}{ta}, ^b{f}{fa}", Self::v(c)));
            }
            Op::Return(w, pc) => match self.kind {
                Kind::Native(_) => self.ret_raw(&Self::v(w)),
                _ => {
                    self.store("%ctx", i64::from(Ctx::RET), &Self::v(w), "i64");
                    let p = self.k64(i64::from(pc));
                    self.store("%ctx", i64::from(Ctx::RET_PC), &p, "i64");
                    let s = self.k64(RETURNED);
                    self.ret_raw(&s);
                }
            },
            Op::Global(_, _, fs) | Op::Deopt(fs) => {
                let e = self.exit(fs, Reason::Always);
                self.line(&format!("llvm.br {e}"));
            }
        }
    }

    fn zero(&mut self) -> String {
        self.k64(0)
    }

    fn o(x: usize) -> i64 {
        i64::try_from(x).unwrap_or(0)
    }

    fn ipred(c: Cond) -> &'static str {
        match c {
            Cond::Lt => "slt",
            Cond::Le => "sle",
            Cond::Gt => "sgt",
            Cond::Ge => "sge",
            Cond::Eq => "eq",
            Cond::Ne => "ne",
        }
    }

    fn fpred(c: Cond) -> &'static str {
        match c {
            Cond::Lt => "olt",
            Cond::Le => "ole",
            Cond::Gt => "ogt",
            Cond::Ge => "oge",
            Cond::Eq => "oeq",
            Cond::Ne => "one",
        }
    }

    /// `(args : types)` a jump from `from` passes to `to`'s phis
    fn block_args(&mut self, from: B, to: B) -> String {
        let blk = &self.g.blocks[to as usize];
        let Some(k) = blk.preds.iter().position(|&p| p == from) else { return String::new() };
        let phis: Vec<V> = blk.insts.iter().copied().filter(|&v| matches!(self.g.op(v), Op::Phi(_))).collect();
        if phis.is_empty() {
            return String::new();
        }
        let vals: Vec<String> = phis.iter().map(|&p| match self.g.op(p) {
            Op::Phi(ins) => Self::v(ins[k]),
            _ => unreachable!("a phi"),
        }).collect();
        let tys: Vec<&str> = phis.iter().map(|&p| ty(self.g.rep(p))).collect();
        format!("({} : {})", vals.join(", "), tys.join(", "))
    }

    fn jump(&mut self, from: B, to: B) {
        let a = self.block_args(from, to);
        self.line(&format!("llvm.br ^b{to}{a}"));
    }

    /// Interpreter entry image slot `i` (see `build`): parameters, registers, context, closure, accumulator,
    /// new.target, the `this` built
    fn param(&mut self, d: &str, i: u32) {
        if let Kind::Native(_) = self.kind {
            let src = match i {
                0 => "%this",
                1 => "%a0",
                2 => "%a1",
                _ => "%a2",
            };
            let z = self.zero();
            self.line(&format!("{d} = llvm.add {src}, {z} : i64"));
            return;
        }
        let (p, r) = (self.params(), self.regs());
        let i = i as usize;
        let (base, off): (&str, i64) = if i < p {
            ("%params", 8 * Self::o(i))
        } else if i < p + r {
            ("%r0", 8 * Self::o(i - p))
        } else {
            match i - p - r {
                0 => ("%r0", -8 * Self::o(FrameSlots::CONTEXT)),
                1 => ("%r0", -8 * Self::o(FrameSlots::CLOSURE)),
                2 => ("%ctx", i64::from(Ctx::ACC)),
                3 => ("%r0", -8 * Self::o(FrameSlots::NEW_TARGET)),
                _ => ("%r0", -8 * Self::o(FrameSlots::NEW_THIS)),
            }
        };
        let p = self.addr(base, off);
        self.line(&format!("{d} = llvm.load {p} : !llvm.ptr -> i64"));
    }

    /// Entry image sizes: set by the caller of `lower` through the first Params (counted here)
    fn params(&self) -> usize {
        self.g.entry_params
    }

    fn regs(&self) -> usize {
        self.g.entry_regs
    }

    /// int32 arithmetic with its guards
    fn int(&mut self, d: &str, op: IntOp, a: V, b: V, fs: Option<Fs>) {
        let (x, y) = (Self::v(a), Self::v(b));
        match op {
            IntOp::Add | IntOp::Sub | IntOp::Mul => {
                let name = match op {
                    IntOp::Add => "add",
                    IntOp::Sub => "sub",
                    _ => "mul",
                };
                let x64 = self.cast("sext", &x, "i32", "i64");
                let y64 = self.cast("sext", &y, "i32", "i64");
                let r64 = self.bin(name, &x64, &y64, "i64");
                self.line(&format!("{d} = llvm.trunc {r64} : i64 to i32"));
                let back = self.cast("sext", d, "i32", "i64");
                let ok = self.icmp("eq", &back, &r64, "i64");
                if let Some(fs) = fs {
                    self.guard(&ok, fs, Reason::Int);
                    if op == IntOp::Mul {
                        // 0 from a negative operand is −0
                        let z = self.k32(0);
                        let nz = self.icmp("ne", d, &z, "i32");
                        let or = self.bin("or", &x, &y, "i32");
                        let pos = self.icmp("sge", &or, &z, "i32");
                        let ok = self.bin("or", &nz, &pos, "i1");
                        self.guard(&ok, fs, Reason::Int);
                    }
                }
            }
            IntOp::Mod => {
                let fs = fs.expect("a modulus has a frame state");
                let z = self.k32(0);
                let m1 = self.k32(-1);
                // A divisor of 0 (NaN) or −1 (INT_MIN overflow; −0 for negatives anyway) hands over
                let nz = self.icmp("ne", &y, &z, "i32");
                self.guard(&nz, fs, Reason::Int);
                let nm = self.icmp("ne", &y, &m1, "i32");
                self.guard(&nm, fs, Reason::Int);
                self.line(&format!("{d} = llvm.srem {x}, {y} : i32"));
                // 0 from a negative dividend is −0
                let rz = self.icmp("ne", d, &z, "i32");
                let pos = self.icmp("sge", &x, &z, "i32");
                let ok = self.bin("or", &rz, &pos, "i1");
                self.guard(&ok, fs, Reason::Int);
            }
            IntOp::And | IntOp::Or | IntOp::Xor => {
                let name = match op {
                    IntOp::And => "and",
                    IntOp::Or => "or",
                    _ => "xor",
                };
                self.line(&format!("{d} = llvm.{name} {x}, {y} : i32"));
            }
            IntOp::Shl | IntOp::Sar | IntOp::Shr => {
                let k = self.k32(31);
                let n = self.bin("and", &y, &k, "i32");
                let name = match op {
                    IntOp::Shl => "shl",
                    IntOp::Sar => "ashr",
                    _ => "lshr",
                };
                self.line(&format!("{d} = llvm.{name} {x}, {n} : i32"));
                if op == IntOp::Shr
                    && let Some(fs) = fs
                {
                    let z = self.k32(0);
                    let ok = self.icmp("sge", d, &z, "i32");
                    self.guard(&ok, fs, Reason::Int);
                }
            }
        }
    }

    /// `d` = the int32 double `f` (bits `bits`) holds; guarded: exact and not −0
    fn float_to_int(&mut self, d: &str, f: &str, bits: &str, fs: Fs) {
        self.line(&format!("{d} = llvm.fptosi {f} : f64 to i32"));
        let back = self.cast("sitofp", d, "i32", "f64");
        let eq = self.t();
        self.line(&format!("{eq} = llvm.fcmp \"oeq\" {f}, {back} : f64"));
        self.guard(&eq, fs, Reason::Int);
        // −0: a zero result from nonzero bits
        let z = self.k32(0);
        let nz = self.icmp("ne", d, &z, "i32");
        let z64 = self.k64(0);
        let pz = self.icmp("eq", bits, &z64, "i64");
        let ok = self.bin("or", &nz, &pz, "i1");
        self.guard(&ok, fs, Reason::Int);
    }

    /// A store into a value cell: plain when neither the old nor the new word is a pointer, else through the barrier
    fn store_cell(&mut self, base: &str, off: i64, w: &str) {
        let addr = if off == 0 { base.to_string() } else {
            let k = self.k64(off);
            self.bin("add", base, &k, "i64")
        };
        let old = self.load(&addr, 0, "i64");
        let min = self.kw(j8framework::paragc::WORD_POINTER_MIN);
        let both = self.bin("or", &old, w, "i64");
        // Either word ≥ the pointer minimum ⇔ their bitwise or is (the minimum's bits are the top 13)
        let ptr = self.icmp("uge", &both, &min, "i64");
        self.tmp += 1;
        let (slow, fast, join) = (format!("^w{}", self.tmp), format!("^f{}", self.tmp), format!("^j{}", self.tmp));
        self.line(&format!("llvm.cond_br {ptr}, {slow}, {fast}"));
        self.out.push_str(&format!("  {fast}:\n"));
        self.store(&addr, 0, w, "i64");
        self.line(&format!("llvm.br {join}"));
        self.out.push_str(&format!("  {slow}:\n"));
        let _ = self.helper(Helper::StoreCell, &[addr.clone(), w.to_string()]);
        self.line(&format!("llvm.br {join}"));
        self.out.push_str(&format!("  {join}:\n"));
    }
}
