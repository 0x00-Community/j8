use alloc::vec::Vec;

use crate::{BytecodeError, OperandScale, OperandSize, OperandTypeInfo, Opcode};

j8macros::wasm_bytecodes!("wasm.json");

impl WasmOperandType {
    pub const fn size(self, scale: OperandScale) -> OperandSize {
        self.info().size(scale)
    }
}

impl Opcode for WasmBytecode {
    fn read(code: &[u8], at: usize) -> Result<(Self, usize), BytecodeError<Self>> {
        let first = *code.get(at).ok_or(BytecodeError::Truncated(at))?;
        let b = Self::from_page(0, first).ok_or(BytecodeError::Bytecode(at))?;
        match b.page_prefix() {
            None => Ok((b, at + 1)),
            Some(page) => {
                let second = *code.get(at + 1).ok_or(BytecodeError::Truncated(at))?;
                Ok((Self::from_page(page, second).ok_or(BytecodeError::Bytecode(at))?, at + 2))
            }
        }
    }

    fn write(self, code: &mut Vec<u8>) {
        let [page, b] = (self as u16).to_be_bytes();
        if let Some(prefix) = Self::PAGES[usize::from(page)] {
            prefix.write(code);
        }
        code.push(b);
    }

    fn operand_infos(self) -> impl ExactSizeIterator<Item = OperandTypeInfo> {
        self.operand_types().iter().map(|t| t.info())
    }

    fn prefix_scale(self) -> Option<OperandScale> {
        match self {
            Self::Wide => Some(OperandScale::Double),
            Self::ExtraWide => Some(OperandScale::Quadruple),
            _ => None,
        }
    }

    fn scale_prefix(scale: OperandScale) -> Option<Self> {
        match scale {
            OperandScale::Single => None,
            OperandScale::Double => Some(Self::Wide),
            OperandScale::Quadruple => Some(Self::ExtraWide),
        }
    }
}
