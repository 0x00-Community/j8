use alloc::vec;
use alloc::vec::Vec;

use j8bytecode::{Bytecode, OperandType};
use j8executor::{JitFunction as Function, JitInsn, JitReg as Reg};

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Bits(Vec<u64>);

impl Bits {
    fn new(n: usize) -> Self {
        Bits(vec![0; n.div_ceil(64)])
    }

    pub fn get(&self, i: usize) -> bool {
        self.0.get(i / 64).is_some_and(|w| w >> (i % 64) & 1 == 1)
    }

    fn set(&mut self, i: usize, on: bool) {
        if let Some(w) = self.0.get_mut(i / 64) {
            if on {
                *w |= 1 << (i % 64);
            } else {
                *w &= !(1 << (i % 64));
            }
        }
    }

    fn union(&mut self, o: &Bits) -> bool {
        let mut changed = false;
        for (a, b) in self.0.iter_mut().zip(&o.0) {
            let n = *a | *b;
            changed |= n != *a;
            *a = n;
        }
        changed
    }

    fn all(n: usize) -> Self {
        let mut b = Bits::new(n);
        for i in 0..n {
            b.set(i, true);
        }
        b
    }
}

pub struct Cfg {
    pub params: usize,
    pub regs: usize,
    pub block_of: Vec<u32>,
    pub starts: Vec<usize>,
    pub loops: Vec<(usize, usize)>,
    pub live: Vec<Bits>,
    pub ok: bool,
}

struct UseDef {
    uses: Vec<usize>,
    defs: Vec<usize>,
}

impl Cfg {
    pub fn new(f: Function<'_>) -> Self {
        let bc = &f.shared().bytecode;
        let params = usize::from(bc.parameter_count);
        let regs = bc.register_count as usize;
        let n = f.len();
        let total = params + regs + 1;
        let acc = params + regs;
        let mut start = vec![false; n + 1];
        start[0] = true;
        let mut loops = Vec::new();
        let mut ok = bc.handlers.is_empty();
        for i in 0..n {
            let d = f.insn(i);
            let b = d.bytecode;
            if b.is_jump() {
                start[d.target] = true;
                start[i + 1] = true;
                if b == Bytecode::JumpLoop {
                    loops.push((d.target, i));
                }
            }
            if matches!(b, Bytecode::Return | Bytecode::Throw | Bytecode::ReThrow | Bytecode::Abort) {
                start[i + 1] = true;
            }
            if matches!(b, Bytecode::SwitchOnSmiNoFeedback | Bytecode::SwitchOnGeneratorState | Bytecode::SuspendGenerator | Bytecode::ResumeGenerator) {
                ok = false;
            }
        }
        let mut starts = Vec::new();
        let mut block_of = vec![0u32; n];
        for i in 0..n {
            if start[i] {
                starts.push(i);
            }
            block_of[i] = u32::try_from(starts.len() - 1).unwrap_or(0);
        }
        let ud: Vec<UseDef> = (0..n).map(|i| Self::use_def(f, f.insn(i), params, acc)).collect();
        let mut live = vec![Bits::new(total); n + 1];
        if !ok {
            live = vec![Bits::all(total); n + 1];
        }
        let mut changed = ok;
        while changed {
            changed = false;
            for i in (0..n).rev() {
                let d = f.insn(i);
                let b = d.bytecode;
                let mut out = Bits::new(total);
                let falls = !(matches!(b, Bytecode::Jump | Bytecode::JumpConstant | Bytecode::JumpLoop | Bytecode::Return | Bytecode::Throw | Bytecode::ReThrow | Bytecode::Abort));
                if falls {
                    out.union(&live[i + 1]);
                }
                if b.is_jump() {
                    out.union(&live[d.target]);
                }
                for &x in &ud[i].defs {
                    out.set(x, false);
                }
                for &x in &ud[i].uses {
                    out.set(x, true);
                }
                if out != live[i] {
                    live[i] = out;
                    changed = true;
                }
            }
        }
        Cfg { params, regs, block_of, starts, loops, live, ok }
    }

    fn use_def(f: Function<'_>, d: JitInsn<'_>, params: usize, acc: usize) -> UseDef {
        let mut u = UseDef { uses: Vec::new(), defs: Vec::new() };
        let reg = |k: i64| -> Option<usize> {
            match f.reg(k) {
                Reg::Param(i) => Some(i),
                Reg::Local(i) => Some(params + i),
                _ => None,
            }
        };
        let types = d.bytecode.operand_types();
        for (j, t) in types.iter().enumerate() {
            let k = d.operands[j];
            let count = || types.get(j + 1).filter(|t| **t == OperandType::RegCount).map_or(0, |_| usize::try_from(d.operands[j + 1]).unwrap_or(0));
            let run = |n: usize, out: &mut Vec<usize>| {
                for x in 0..n {
                    if let Some(r) = reg(k + i64::try_from(x).unwrap_or(0)) {
                        out.push(r);
                    }
                }
            };
            match t {
                OperandType::Reg => run(1, &mut u.uses),
                OperandType::RegList => run(count(), &mut u.uses),
                OperandType::RegPair => run(2, &mut u.uses),
                OperandType::RegOut => run(1, &mut u.defs),
                OperandType::RegOutList => run(count(), &mut u.defs),
                OperandType::RegOutPair => run(2, &mut u.defs),
                OperandType::RegOutTriple => run(3, &mut u.defs),
                OperandType::RegInOut => {
                    run(1, &mut u.uses);
                    run(1, &mut u.defs);
                }
                _ => {}
            }
        }
        if d.bytecode == Bytecode::Star {
            u.uses.clear();
            u.defs.clear();
            if let Some(r) = reg(d.operands[0]) {
                u.defs.push(r);
            }
        }
        if d.bytecode.reads_accumulator() {
            u.uses.push(acc);
        }
        if d.bytecode.writes_accumulator() && !d.bytecode.reads_accumulator() {
            u.defs.push(acc);
        }
        u
    }

    pub fn loop_at(&self, at: usize) -> Option<(usize, usize)> {
        self.loops.iter().copied().find(|&(_, j)| j == at)
    }

    pub fn enclosing(&self, pc: usize) -> Vec<(usize, usize)> {
        let mut v: Vec<(usize, usize)> = self.loops.iter().copied().filter(|&(h, j)| h <= pc && pc <= j).collect();
        v.sort_by_key(|&(h, j)| j - h);
        v
    }
}
