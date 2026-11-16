//! Post-compile peephole fusion: a few instructions that often appear together become one, saving dispatches.
//!
//! Only groups whose later instructions are not jump targets are fused: a target can only be the first instruction
//! of the group, and jump targets are rewritten to the new indices. A fused instruction means exactly those
//! instructions executed in order, so when the JIT bails out to the interpreter midway, redoing the whole fused
//! instruction is the same (the leading ones only write acc).
use alloc::vec;
use alloc::vec::Vec;

use crate::{Function, Insn};

/// The original instructions a fused instruction splits back into (at most three).
pub struct Unfused {
    parts: [Insn; 3],
    len: usize,
}

impl core::ops::Deref for Unfused {
    type Target = [Insn];

    fn deref(&self) -> &[Insn] {
        &self.parts[..self.len]
    }
}

impl From<(Insn, Insn)> for Unfused {
    fn from((a, b): (Insn, Insn)) -> Self {
        Self { parts: [a, b, Insn::LdaUndefined], len: 2 }
    }
}

impl From<(Insn, Insn, Insn)> for Unfused {
    fn from((a, b, c): (Insn, Insn, Insn)) -> Self {
        Self { parts: [a, b, c], len: 3 }
    }
}

impl Insn {
    /// Splits a fused instruction back into the originals; `None` if it is not a fused instruction.
    #[must_use]
    pub fn unfused(self) -> Option<Unfused> {
        Some(match self {
            Insn::BinarySmi(op, x, n) => (Insn::LdaSmi(n), Insn::Binary(op, x)).into(),
            Insn::CompareSmi(op, x, n) => (Insn::LdaSmi(n), Insn::Compare(op, x)).into(),
            Insn::BinaryReg(op, x, y) => (Insn::Ldar(y), Insn::Binary(op, x)).into(),
            Insn::CompareReg(op, x, y) => (Insn::Ldar(y), Insn::Compare(op, x)).into(),
            Insn::Mov { dst, src } => (Insn::Ldar(src), Insn::Star(dst)).into(),
            Insn::IncReg(x) => (Insn::Ldar(x), Insn::Inc, Insn::Star(x)).into(),
            Insn::DecReg(x) => (Insn::Ldar(x), Insn::Dec, Insn::Star(x)).into(),
            _ => return None,
        })
    }

    /// What can be fused into one starting at `at`: (fused instruction, number consumed).
    /// `inner(k)`: instruction `at + k` exists and is not a jump target.
    fn fuse_at(insns: &[Insn], at: usize, inner: impl Fn(usize) -> bool) -> Option<(Insn, usize)> {
        let next = |k: usize| inner(k).then(|| insns[at + k]);
        match (insns[at], next(1), next(2)) {
            (Insn::Ldar(x), Some(Insn::Inc), Some(Insn::Star(y))) if x == y => Some((Insn::IncReg(x), 3)),
            (Insn::Ldar(x), Some(Insn::Dec), Some(Insn::Star(y))) if x == y => Some((Insn::DecReg(x), 3)),
            // Store then immediately load the same register: acc already holds it
            (Insn::Star(x), Some(Insn::Ldar(y)), _) if x == y => Some((Insn::Star(x), 2)),
            (Insn::Ldar(src), Some(Insn::Star(dst)), _) => Some((Insn::Mov { dst, src }, 2)),
            (Insn::LdaSmi(n), Some(Insn::Binary(op, x)), _) => Some((Insn::BinarySmi(op, x, n), 2)),
            (Insn::LdaSmi(n), Some(Insn::Compare(op, x)), _) => Some((Insn::CompareSmi(op, x, n), 2)),
            (Insn::Ldar(y), Some(Insn::Binary(op, x)), _) => Some((Insn::BinaryReg(op, x, y), 2)),
            (Insn::Ldar(y), Some(Insn::Compare(op, x)), _) => Some((Insn::CompareReg(op, x, y), 2)),
            _ => None,
        }
    }
}

impl Function {
    /// Peephole fusion (see the module docs).
    pub fn fuse(&mut self) {
        let n = self.insns.len();
        let mut target = vec![false; n + 1];
        for mut i in self.insns.iter().copied() {
            if let Some(l) = i.target_mut() {
                target[l.0 as usize] = true;
            }
        }
        let mut out = Vec::with_capacity(n);
        // Old index ⇒ new index (only the first instruction of a group and the end can be jumped to)
        let mut moved = vec![0u32; n + 1];
        let mut at = 0;
        while at < n {
            moved[at] = u32::try_from(out.len()).expect("fewer than 2^32 instructions");
            let inner = |k: usize| at + k < n && !target[at + k];
            let (insn, used) = Insn::fuse_at(&self.insns, at, inner).unwrap_or((self.insns[at], 1));
            // Copying back between two registers that were just copied changes nothing (acc already has the value):
            // drop it unless it is a jump target
            let undo = matches!((out.last(), insn), (Some(&Insn::Mov { dst, src }), Insn::Mov { dst: d2, src: s2 }) if d2 == src && s2 == dst);
            if !(undo && !target[at]) {
                out.push(insn);
            }
            at += used;
        }
        moved[n] = u32::try_from(out.len()).expect("fewer than 2^32 instructions");
        for i in &mut out {
            if let Some(l) = i.target_mut() {
                l.0 = moved[l.0 as usize];
            }
        }
        self.insns = out;
    }
}
