use alloc::boxed::Box;
use alloc::vec::Vec;

use crate::asm::{AsmError, Operand, Operands};

j8macros::enums!("insn.json");

impl PartialEq for Constant {
    fn eq(&self, o: &Self) -> bool {
        match (self, o) {
            (Constant::Number(a), Constant::Number(b)) => a.to_bits() == b.to_bits(),
            (Constant::String(a), Constant::String(b)) | (Constant::BigInt(a), Constant::BigInt(b)) => a == b,
            _ => false,
        }
    }
}

impl From<bool> for Mode {
    fn from(strict: bool) -> Self {
        if strict { Self::Strict } else { Self::Sloppy }
    }
}

impl From<Mode> for bool {
    fn from(m: Mode) -> Self {
        m == Mode::Strict
    }
}

impl Function {
    /// A string in the constant pool (a property name / global name).
    pub fn name_at(&self, k: Const) -> &str {
        match &self.consts[k.0 as usize] {
            Constant::String(s) => s,
            Constant::Number(_) | Constant::BigInt(_) => unreachable!("a name constant is a string"),
        }
    }
}
