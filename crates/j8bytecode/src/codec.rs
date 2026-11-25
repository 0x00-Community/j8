use alloc::vec::Vec;
use core::marker::PhantomData;

use crate::{Bytecode, OperandTypeInfo};

pub trait Opcode: Copy {
    fn read(code: &[u8], at: usize) -> Result<(Self, usize), BytecodeError<Self>>;
    fn write(self, code: &mut Vec<u8>);
    fn operand_infos(self) -> impl ExactSizeIterator<Item = OperandTypeInfo>;
    fn prefix_scale(self) -> Option<OperandScale>;
    fn scale_prefix(scale: OperandScale) -> Option<Self>;
}

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

    pub const fn size(self, scale: OperandScale) -> OperandSize {
        match (self.is_scalable(), scale) {
            (true, OperandScale::Double) => OperandSize::Short,
            (true, OperandScale::Quadruple) => OperandSize::Quad,
            _ => self.unscaled_size(),
        }
    }

    pub const fn unscaled_size(self) -> OperandSize {
        match self {
            Self::None => OperandSize::None,
            Self::ScalableSignedByte | Self::ScalableUnsignedByte | Self::FixedUnsignedByte => OperandSize::Byte,
            Self::FixedUnsignedShort => OperandSize::Short,
        }
    }
}


pub type OperandValue = i64;

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Decoded<C = Bytecode> {
    pub offset: usize,
    pub scale: OperandScale,
    pub bytecode: C,
    pub operands: Operands,
    pub next: usize,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub struct Operands {
    values: [OperandValue; Operands::MAX],
    len: u8,
}

impl Operands {
    pub const MAX: usize = 6;

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

impl<C> Decoded<C> {
    pub fn start(&self) -> usize {
        self.offset + usize::from(self.scale != OperandScale::Single)
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum BytecodeError<C = Bytecode> {
    Bytecode(usize),
    Truncated(usize),
    Arity(C, usize),
}

pub struct Decoder<'c, C = Bytecode> {
    code: &'c [u8],
    at: usize,
    opcode: PhantomData<C>,
}

impl<'c, C> From<&'c [u8]> for Decoder<'c, C> {
    fn from(code: &'c [u8]) -> Self {
        Self { code, at: 0, opcode: PhantomData }
    }
}

impl<C: Opcode> Decoder<'_, C> {
    pub fn operand(code: &[u8], at: usize, ty: OperandTypeInfo, scale: OperandScale) -> Option<OperandValue> {
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

    pub fn decode(code: &[u8], at: usize) -> Result<Decoded<C>, BytecodeError<C>> {
        let (first, after) = C::read(code, at)?;
        let (scale, start) = match first.prefix_scale() {
            Some(s) => (s, after),
            None => (OperandScale::Single, at),
        };
        let (bytecode, mut next) = C::read(code, start)?;
        let types = bytecode.operand_infos();
        let mut operands = Operands::default();
        for (i, ty) in types.enumerate() {
            operands.values[i] = Self::operand(code, next, ty, scale).ok_or(BytecodeError::Truncated(at))?;
            next += usize::from(ty.size(scale));
            operands.len = u8::try_from(i + 1).unwrap_or(0);
        }
        Ok(Decoded { offset: at, scale, bytecode, operands, next })
    }
}

impl<C: Opcode> Iterator for Decoder<'_, C> {
    type Item = Result<Decoded<C>, BytecodeError<C>>;

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
    pub fn scale<C: Opcode>(bytecode: C, operands: &[OperandValue]) -> OperandScale {
        bytecode
            .operand_infos()
            .zip(operands)
            .filter(|(t, _)| t.is_scalable())
            .map(|(t, &v)| if t.is_unsigned() { OperandScale::for_unsigned(u32::try_from(v).unwrap_or(u32::MAX)) } else { OperandScale::for_signed(i32::try_from(v).unwrap_or(i32::MAX)) })
            .max()
            .unwrap_or(OperandScale::Single)
    }

    pub fn emit<C: Opcode>(&mut self, bytecode: C, operands: &[OperandValue]) -> Result<usize, BytecodeError<C>> {
        if bytecode.operand_infos().len() != operands.len() {
            return Err(BytecodeError::Arity(bytecode, operands.len()));
        }
        let at = self.code.len();
        let scale = Self::scale(bytecode, operands);
        if let Some(p) = C::scale_prefix(scale) {
            p.write(&mut self.code);
        }
        bytecode.write(&mut self.code);
        for (ty, &v) in bytecode.operand_infos().zip(operands) {
            let n = usize::from(ty.size(scale));
            #[expect(clippy::cast_possible_truncation, clippy::cast_sign_loss, reason = "the scale was chosen so the value fits n bytes")]
            let raw = (v as u32).to_le_bytes();
            self.code.extend_from_slice(&raw[..n]);
        }
        Ok(at)
    }
}
