use alloc::vec::Vec;

j8macros::bytecodes!("v8.json");

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
#[repr(u8)]
pub enum OperandScale {
    Single = 1,
    Double = 2,
    Quadruple = 4,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
#[repr(u8)]
pub enum OperandSize {
    None = 0,
    Byte = 1,
    Short = 2,
    Quad = 4,
}

impl From<OperandScale> for OperandSize {
    fn from(s: OperandScale) -> Self {
        match s {
            OperandScale::Single => Self::Byte,
            OperandScale::Double => Self::Short,
            OperandScale::Quadruple => Self::Quad,
        }
    }
}

impl From<OperandSize> for usize {
    fn from(s: OperandSize) -> Self {
        s as usize
    }
}

impl OperandScale {
    pub const fn prefix(self) -> Option<Bytecode> {
        match self {
            Self::Single => None,
            Self::Double => Some(Bytecode::Wide),
            Self::Quadruple => Some(Bytecode::ExtraWide),
        }
    }

    pub const fn for_signed(v: i32) -> Self {
        if v >= i8::MIN as i32 && v <= i8::MAX as i32 {
            Self::Single
        } else if v >= i16::MIN as i32 && v <= i16::MAX as i32 {
            Self::Double
        } else {
            Self::Quadruple
        }
    }

    pub const fn for_unsigned(v: u32) -> Self {
        if v <= u8::MAX as u32 {
            Self::Single
        } else if v <= u16::MAX as u32 {
            Self::Double
        } else {
            Self::Quadruple
        }
    }
}

impl OperandTypeInfo {
    pub const fn is_scalable(self) -> bool {
        matches!(self, Self::ScalableSignedByte | Self::ScalableUnsignedByte)
    }

    pub const fn is_unsigned(self) -> bool {
        matches!(self, Self::ScalableUnsignedByte | Self::FixedUnsignedByte | Self::FixedUnsignedShort)
    }

    pub const fn unscaled_size(self) -> OperandSize {
        match self {
            Self::None => OperandSize::None,
            Self::ScalableSignedByte | Self::ScalableUnsignedByte | Self::FixedUnsignedByte => OperandSize::Byte,
            Self::FixedUnsignedShort => OperandSize::Short,
        }
    }
}

impl OperandType {
    pub const fn size(self, scale: OperandScale) -> OperandSize {
        let info = self.info();
        match (info.is_scalable(), scale) {
            (true, OperandScale::Double) => OperandSize::Short,
            (true, OperandScale::Quadruple) => OperandSize::Quad,
            _ => info.unscaled_size(),
        }
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

pub type OperandValue = i64;

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Decoded {
    pub offset: usize,
    pub scale: OperandScale,
    pub bytecode: Bytecode,
    pub operands: Operands,
    pub next: usize,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub struct Operands {
    values: [OperandValue; Operands::MAX],
    len: u8,
}

impl Operands {
    pub const MAX: usize = 5;

    #[inline]
    pub fn all(&self) -> &[OperandValue; Self::MAX] {
        &self.values
    }
}

impl core::ops::Deref for Operands {
    type Target = [OperandValue];

    #[inline]
    fn deref(&self) -> &[OperandValue] {
        &self.values[..usize::from(self.len)]
    }
}

impl core::ops::DerefMut for Operands {
    #[inline]
    fn deref_mut(&mut self) -> &mut [OperandValue] {
        &mut self.values[..usize::from(self.len)]
    }
}

impl From<&[OperandValue]> for Operands {
    fn from(v: &[OperandValue]) -> Self {
        let mut o = Self::default();
        let n = v.len().min(Self::MAX);
        o.values[..n].copy_from_slice(&v[..n]);
        o.len = u8::try_from(n).unwrap_or(0);
        o
    }
}

impl Decoded {
    pub fn start(&self) -> usize {
        self.offset + usize::from(self.scale != OperandScale::Single)
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum BytecodeError {
    Bytecode(usize),
    Truncated(usize),
    Arity(Bytecode, usize),
}

pub struct Decoder<'c> {
    code: &'c [u8],
    at: usize,
}

impl<'c> From<&'c [u8]> for Decoder<'c> {
    fn from(code: &'c [u8]) -> Self {
        Self { code, at: 0 }
    }
}

impl Decoder<'_> {
    pub fn operand(code: &[u8], at: usize, ty: OperandType, scale: OperandScale) -> Option<OperandValue> {
        let n = usize::from(ty.size(scale));
        let b = code.get(at..at + n)?;
        let mut raw = [0u8; 4];
        raw[..n].copy_from_slice(b);
        let u = u32::from_le_bytes(raw);
        Some(match (ty.is_unsigned(), n) {
            (true, _) => OperandValue::from(u),
            (false, 1) => OperandValue::from(i8::from_le_bytes([raw[0]])),
            (false, 2) => OperandValue::from(i16::from_le_bytes([raw[0], raw[1]])),
            (false, _) => OperandValue::from(i32::from_le_bytes(raw)),
        })
    }

    pub fn decode(code: &[u8], at: usize) -> Result<Decoded, BytecodeError> {
        let first = Bytecode::from_u8(*code.get(at).ok_or(BytecodeError::Truncated(at))?).ok_or(BytecodeError::Bytecode(at))?;
        let (scale, start) = match first.prefix_scale() {
            Some(s) => (s, at + 1),
            None => (OperandScale::Single, at),
        };
        let bytecode = Bytecode::from_u8(*code.get(start).ok_or(BytecodeError::Truncated(at))?).ok_or(BytecodeError::Bytecode(start))?;
        let mut operands = Operands::default();
        let mut next = start + 1;
        for (i, &ty) in bytecode.operand_types().iter().enumerate() {
            operands.values[i] = Self::operand(code, next, ty, scale).ok_or(BytecodeError::Truncated(at))?;
            next += usize::from(ty.size(scale));
        }
        operands.len = u8::try_from(bytecode.operand_types().len()).unwrap_or(0);
        Ok(Decoded { offset: at, scale, bytecode, operands, next })
    }
}

impl Iterator for Decoder<'_> {
    type Item = Result<Decoded, BytecodeError>;

    fn next(&mut self) -> Option<Self::Item> {
        (self.at < self.code.len()).then(|| {
            let d = Self::decode(self.code, self.at);
            self.at = d.as_ref().map_or(self.code.len(), |d| d.next);
            d
        })
    }
}

#[derive(Default)]
pub struct Encoder {
    pub code: Vec<u8>,
}

impl Encoder {
    pub fn scale(bytecode: Bytecode, operands: &[OperandValue]) -> OperandScale {
        bytecode
            .operand_types()
            .iter()
            .zip(operands)
            .filter(|(t, _)| t.info().is_scalable())
            .map(|(t, &v)| if t.is_unsigned() { OperandScale::for_unsigned(u32::try_from(v).unwrap_or(u32::MAX)) } else { OperandScale::for_signed(i32::try_from(v).unwrap_or(i32::MAX)) })
            .max()
            .unwrap_or(OperandScale::Single)
    }

    pub fn emit(&mut self, bytecode: Bytecode, operands: &[OperandValue]) -> Result<usize, BytecodeError> {
        let types = bytecode.operand_types();
        if types.len() != operands.len() {
            return Err(BytecodeError::Arity(bytecode, operands.len()));
        }
        let at = self.code.len();
        let scale = Self::scale(bytecode, operands);
        if let Some(p) = scale.prefix() {
            self.code.push(p as u8);
        }
        self.code.push(bytecode as u8);
        for (&ty, &v) in types.iter().zip(operands) {
            let n = usize::from(ty.size(scale));
            #[expect(clippy::cast_possible_truncation, clippy::cast_sign_loss, reason = "the scale was chosen so the value fits n bytes")]
            let raw = (v as u32).to_le_bytes();
            self.code.extend_from_slice(&raw[..n]);
        }
        Ok(at)
    }
}
