//! Function code: assembling a `Draft` into bytes, and reading instructions back at byte offsets.
//!
//! An instruction is its `Binary` encoding: a one-byte opcode (the variant's position in insn.json), then the fields
//! fixed-width little-endian. The program counter is a byte offset, and so are jump targets. After the last
//! instruction come `Insn::PAD` zero bytes, so decoding can always read a fixed window (`Insn::WINDOW`) with one
//! bounds check; [`Function::end`] is where the instructions end.
use alloc::boxed::Box;
use alloc::vec;
use alloc::vec::Vec;

use crate::{BinError, Binary, Draft, Function, IcSlot, Insn, Reader};

/// Assembling: jump targets go from instruction indices to byte offsets, inline cache slots are numbered from 0.
impl From<Draft> for Box<[u8]> {
    fn from(draft: Draft) -> Self {
        let mut at = Vec::with_capacity(draft.0.len() + 1);
        let mut out = Vec::new();
        for i in &draft.0 {
            at.push(u32::try_from(out.len()).expect("code under 4 GiB"));
            i.encode(&mut out);
        }
        at.push(u32::try_from(out.len()).expect("code under 4 GiB"));
        out.clear();
        let mut slot = 0;
        for mut i in draft.0 {
            if let Some(l) = i.target_mut() {
                l.0 = at[l.0 as usize];
            }
            if let Some(s) = i.ic_mut() {
                *s = IcSlot(slot);
                slot += 1;
            }
            i.encode(&mut out);
        }
        out.resize(out.len() + Insn::PAD, 0);
        out.into()
    }
}

/// The instructions of a function's code with their byte offsets.
pub struct Insns<'f> {
    func: &'f Function,
    pc: usize,
}

impl Iterator for Insns<'_> {
    type Item = (u32, Insn);

    fn next(&mut self) -> Option<(u32, Insn)> {
        (self.pc < self.func.end()).then(|| {
            let at = u32::try_from(self.pc).expect("code under 4 GiB");
            let insn;
            (insn, self.pc) = self.func.insn_at(self.pc);
            (at, insn)
        })
    }
}

impl Function {
    /// The instruction at byte offset `pc`, and the offset of the next one. The code must be assembled from a `Draft`
    /// or checked by decoding (`Module::try_from`); panics otherwise.
    #[expect(clippy::inline_always, reason = "the interpreter's match on the result must merge with the decoder's on the opcode")]
    #[inline(always)]
    pub fn insn_at(&self, pc: usize) -> (Insn, usize) {
        Insn::decode_at(&self.code, pc)
    }

    /// Where the instructions end (the padding starts)
    pub fn end(&self) -> usize {
        self.code.len().saturating_sub(Insn::PAD)
    }

    /// Every instruction with its byte offset
    pub fn insns(&self) -> Insns<'_> {
        Insns { func: self, pc: 0 }
    }

    /// How many inline cache slots the code uses
    pub fn ic_slots(&self) -> usize {
        self.insns().filter_map(|(_, mut i)| i.ic_mut().map(|s| s.0 as usize + 1)).max().unwrap_or(0)
    }

    /// Checks code from outside (`index` is the function's position, for errors): the padding is there, every
    /// instruction decodes and every jump lands on the start of one (or the end).
    pub(crate) fn verify(&self, index: usize) -> Result<(), BinError> {
        let end = self.end();
        if self.code.len() < Insn::PAD || self.code[end..].iter().any(|&b| b != 0) {
            return Err(BinError::Code(index, end));
        }
        let mut starts = vec![false; end + 1];
        let mut targets = Vec::new();
        let mut r = Reader::from(&self.code[..end]);
        while r.left() > 0 {
            let at = end - r.left();
            starts[at] = true;
            let mut insn = Insn::decode(&mut r).map_err(|_| BinError::Code(index, at))?;
            targets.extend(insn.target_mut().map(|l| l.0));
        }
        starts[end] = true;
        match targets.into_iter().find(|&t| !starts.get(t as usize).copied().unwrap_or(false)) {
            Some(t) => Err(BinError::Target(index, t)),
            None => Ok(()),
        }
    }
}
