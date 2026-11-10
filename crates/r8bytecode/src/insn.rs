use std::fmt;

use r8::{BinaryOp, FuncId};

/// 寄存器（函数帧里的槽位）。
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct Reg(pub u16);

/// 常量池里的一项。
#[derive(Clone, Debug, PartialEq)]
pub enum Constant {
    Number(f64),
    String(Box<str>),
}

/// 一条指令：结果都进累加器 `acc`。
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Insn {
    /// acc = 常量池[k]
    LdaConst(u32),
    LdaUndefined,
    LdaNull,
    LdaBool(bool),
    /// reg = acc
    Star(Reg),
    /// acc = reg
    Ldar(Reg),
    /// acc = reg op acc
    Binary(BinaryOp, Reg),
    /// acc = -acc
    Negate,
    Return,
}

/// 一个函数：指令、常量池、寄存器数。
#[derive(Clone, Debug, Default)]
pub struct Function {
    pub insns: Vec<Insn>,
    pub consts: Vec<Constant>,
    pub regs: u16,
}

/// 一个脚本编出的全部函数；`FuncId(0)` 是脚本本身。
#[derive(Clone, Debug, Default)]
pub struct Module {
    pub functions: Vec<Function>,
}

impl Module {
    pub const MAIN: FuncId = FuncId(0);

    pub fn function(&self, f: FuncId) -> &Function {
        &self.functions[f.0 as usize]
    }
}

impl fmt::Display for Insn {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Insn::LdaConst(k) => write!(f, "lda.const #{k}"),
            Insn::LdaUndefined => f.write_str("lda.undefined"),
            Insn::LdaNull => f.write_str("lda.null"),
            Insn::LdaBool(b) => write!(f, "lda.bool {b}"),
            Insn::Star(r) => write!(f, "star r{}", r.0),
            Insn::Ldar(r) => write!(f, "ldar r{}", r.0),
            Insn::Binary(op, r) => write!(f, "{} r{}", op.name(), r.0),
            Insn::Negate => f.write_str("negate"),
            Insn::Return => f.write_str("return"),
        }
    }
}

impl fmt::Display for Module {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        for (i, func) in self.functions.iter().enumerate() {
            writeln!(f, "function #{i} (regs {}):", func.regs)?;
            for (k, c) in func.consts.iter().enumerate() {
                writeln!(f, "  const #{k} = {c:?}")?;
            }
            for insn in &func.insns {
                writeln!(f, "  {insn}")?;
            }
        }
        Ok(())
    }
}
