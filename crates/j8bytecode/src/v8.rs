use alloc::vec::Vec;

use crate::{BytecodeError, OperandScale, OperandSize, Opcode};

j8macros::bytecodes!("v8.json");

impl OperandType {
    pub const fn size(self, scale: OperandScale) -> OperandSize {
        self.info().size(scale)
    }

    pub const fn is_unsigned(self) -> bool {
        self.info().is_unsigned()
    }

    pub const fn is_register(self) -> bool {
        matches!(self, Self::Reg | Self::RegList | Self::RegPair | Self::RegOut | Self::RegOutList | Self::RegOutPair | Self::RegOutTriple | Self::RegInOut)
    }
}

impl Bytecode {
    pub const FIRST_SHORT_STAR: Bytecode = Bytecode::Star15;
    pub const LAST_SHORT_STAR: Bytecode = Bytecode::Star0;
    pub const SHORT_STAR_COUNT: u8 = Bytecode::LAST_SHORT_STAR as u8 - Bytecode::FIRST_SHORT_STAR as u8 + 1;

    pub const fn prefix_scale(self) -> Option<OperandScale> {
        match self {
            Self::Wide | Self::DebugBreakWide => Some(OperandScale::Double),
            Self::ExtraWide | Self::DebugBreakExtraWide => Some(OperandScale::Quadruple),
            _ => None,
        }
    }

    pub const fn is_short_star(self) -> bool {
        self as u8 >= Self::FIRST_SHORT_STAR as u8 && self as u8 <= Self::LAST_SHORT_STAR as u8
    }

    pub const fn is_jump(self) -> bool {
        self as u8 >= Self::JumpLoop as u8 && self as u8 <= Self::JumpIfForInDone as u8
    }

    pub const fn is_jump_constant(self) -> bool {
        self as u8 >= Self::JumpConstant as u8 && self as u8 <= Self::JumpIfToBooleanFalseConstant as u8
    }

    pub fn short_star(r: Register) -> Option<Self> {
        match u8::try_from(r.0) {
            Ok(i) if i < Self::SHORT_STAR_COUNT => Self::from_u8(Self::LAST_SHORT_STAR as u8 - i),
            _ => None,
        }
    }

    pub const fn short_star_register(self) -> Register {
        Register((Self::LAST_SHORT_STAR as u8 - self as u8) as i32)
    }

    pub fn operand_offset(self, i: usize, scale: OperandScale) -> usize {
        1 + self.operand_types()[..i].iter().map(|t| usize::from(t.size(scale))).sum::<usize>()
    }

    pub fn size(self, scale: OperandScale) -> usize {
        self.operand_offset(self.operand_types().len(), scale)
    }

    pub const fn reads_accumulator(self) -> bool {
        self.implicit_register_use().0 & ImplicitRegisterUse::ReadAccumulator.0 != 0
    }

    pub const fn writes_accumulator(self) -> bool {
        self.implicit_register_use().0 & ImplicitRegisterUse::WriteAccumulator.0 != 0
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct Register(pub i32);

impl Register {
    pub const REGISTER_FILE_FROM_FP: i32 = -7;
    pub const FIRST_PARAM_FROM_FP: i32 = 2;
    pub const CONTEXT_FROM_FP: i32 = -1;
    pub const FUNCTION_FROM_FP: i32 = -2;
    pub const ARGC_FROM_FP: i32 = -3;
    pub const BYTECODE_ARRAY_FROM_FP: i32 = -4;
    pub const BYTECODE_OFFSET_FROM_FP: i32 = -5;
    pub const FEEDBACK_VECTOR_FROM_FP: i32 = -6;

    pub const fn at_fp_slot(slot: i32) -> Self {
        Self(Self::REGISTER_FILE_FROM_FP - slot)
    }

    pub const fn function_closure() -> Self {
        Self::at_fp_slot(Self::FUNCTION_FROM_FP)
    }

    pub const fn current_context() -> Self {
        Self::at_fp_slot(Self::CONTEXT_FROM_FP)
    }

    pub const fn parameter(i: i32) -> Self {
        Self::at_fp_slot(Self::FIRST_PARAM_FROM_FP + i)
    }

    pub const fn receiver() -> Self {
        Self::parameter(0)
    }

    pub const fn parameter_index(self) -> Option<i32> {
        let i = self.fp_slot() - Self::FIRST_PARAM_FROM_FP;
        if i >= 0 { Some(i) } else { None }
    }

    pub const fn fp_slot(self) -> i32 {
        Self::REGISTER_FILE_FROM_FP - self.0
    }
}

impl From<Register> for i32 {
    fn from(r: Register) -> Self {
        r.fp_slot()
    }
}

impl From<i32> for Register {
    fn from(operand: i32) -> Self {
        Self::at_fp_slot(operand)
    }
}


impl Opcode for Bytecode {
    fn read(code: &[u8], at: usize) -> Result<(Self, usize), BytecodeError<Self>> {
        let b = Self::from_u8(*code.get(at).ok_or(BytecodeError::Truncated(at))?).ok_or(BytecodeError::Bytecode(at))?;
        Ok((b, at + 1))
    }

    fn write(self, code: &mut Vec<u8>) {
        code.push(self as u8);
    }

    fn operand_infos(self) -> impl ExactSizeIterator<Item = OperandTypeInfo> {
        self.operand_types().iter().map(|t| t.info())
    }

    fn prefix_scale(self) -> Option<OperandScale> {
        Bytecode::prefix_scale(self)
    }

    fn scale_prefix(scale: OperandScale) -> Option<Self> {
        match scale {
            OperandScale::Single => None,
            OperandScale::Double => Some(Self::Wide),
            OperandScale::Quadruple => Some(Self::ExtraWide),
        }
    }
}
