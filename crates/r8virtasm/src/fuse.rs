//! 编译后的窥孔融合：几条常连着出现的指令并成一条，少几次分派。
//!
//! 只融合「后面几条都不是跳转落点」的组合：落点只可能在组的第一条，跳转目标按新下标改写。
//! 融合指令的语义就是那几条依次执行，所以 JIT 从中途退回解释器时整条重做也一样（前几条只写 acc）。
use alloc::vec;
use alloc::vec::Vec;

use crate::{Function, Insn};

/// 一条融合指令拆回的原指令（至多三条）。
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
    /// 融合指令拆回原来的几条；不是融合指令给 `None`。
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

    /// 从 `at` 起能并成一条的：（融合后的指令, 吃掉几条）。`inner(k)`：第 `at + k` 条存在且不是跳转落点。
    fn fuse_at(insns: &[Insn], at: usize, inner: impl Fn(usize) -> bool) -> Option<(Insn, usize)> {
        let next = |k: usize| inner(k).then(|| insns[at + k]);
        match (insns[at], next(1), next(2)) {
            (Insn::Ldar(x), Some(Insn::Inc), Some(Insn::Star(y))) if x == y => Some((Insn::IncReg(x), 3)),
            (Insn::Ldar(x), Some(Insn::Dec), Some(Insn::Star(y))) if x == y => Some((Insn::DecReg(x), 3)),
            // 存完马上读回同一个寄存器：acc 本来就是它
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
    /// 窥孔融合（见模块文档）。
    pub fn fuse(&mut self) {
        let n = self.insns.len();
        let mut target = vec![false; n + 1];
        for mut i in self.insns.iter().copied() {
            if let Some(l) = i.target_mut() {
                target[l.0 as usize] = true;
            }
        }
        let mut out = Vec::with_capacity(n);
        // 旧下标 ⇒ 新下标（只有组的第一条与末尾会被跳到）
        let mut moved = vec![0u32; n + 1];
        let mut at = 0;
        while at < n {
            moved[at] = u32::try_from(out.len()).expect("fewer than 2^32 instructions");
            let inner = |k: usize| at + k < n && !target[at + k];
            let (insn, used) = Insn::fuse_at(&self.insns, at, inner).unwrap_or((self.insns[at], 1));
            // 刚互相复制过的两个寄存器再反着复制一遍：什么都不改（acc 也已经是这个值），不是落点就丢掉
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
