use alloc::boxed::Box;
use alloc::vec::Vec;
use core::fmt;

use j8framework::{BinaryOp, CompareOp, UnaryOp};

/// Register: a slot in the function frame. Calling convention: `r0` is `this`, `r1` is the callee itself,
/// parameters start at `r2`.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct Reg(pub u16);

impl Reg {
    pub const THIS: Reg = Reg(0);
    pub const CALLEE: Reg = Reg(1);
    /// The first parameter
    pub const PARAMS: u16 = 2;
}

/// Jump target: an instruction index.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct Label(pub u32);

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

/// Constant pool index.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct Const(pub u32);

/// Code mode: sloppy / strict.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Mode {
    #[default]
    Sloppy,
    Strict,
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

/// A binding visible at a direct eval: name, slot in its environment, whether it is `const`, whether it needs a
/// TDZ check.
#[derive(Clone, Debug)]
pub struct EvalName {
    pub name: Box<str>,
    pub slot: u16,
    pub constant: bool,
    pub tdz: bool,
    /// The own name of a named function expression: assignment is silently ignored in sloppy mode, throws TypeError
    /// in strict mode
    pub self_name: bool,
}

/// The scope table at a direct eval: innermost first, one level per environment on the environment chain.
#[derive(Clone, Debug, Default)]
pub struct EvalScope(pub Vec<Vec<EvalName>>);

/// A function: name, parameter count, register count, instructions, constant pool.
#[derive(Clone, Debug, Default)]
pub struct Function {
    pub name: Box<str>,
    pub kind: FunctionKind,
    /// Keep all arguments in the frame on entry (for default derived constructors and rest parameters)
    pub keeps_args: bool,
    /// Generator function: a call does not run it, it returns a generator
    pub generator: bool,
    /// async function: a call runs it up to the first `await` and returns a Promise
    pub asynchronous: bool,
    /// Strict mode: `this` is not replaced by the global object, failed assignments / deletes throw
    pub mode: Mode,
    pub params: u16,
    /// The function's `length`: the number of parameters before the first one with a default value (or the rest
    /// parameter)
    pub length: u16,
    pub regs: u16,
    pub insns: Vec<Insn>,
    pub consts: Vec<Constant>,
    /// Scope tables of every direct eval in the function (the `scope` of `CallEval` indexes this)
    pub scopes: Vec<EvalScope>,
    /// Mapped arguments: the parameter environment slot for the i-th argument (only for sloppy mode with simple
    /// parameters)
    pub mapped_params: Vec<Option<u16>>,
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

/// All functions compiled from one script; function 0 is the script itself.
#[derive(Clone, Debug, Default)]
pub struct Module {
    pub functions: Vec<Function>,
}

impl Module {
    /// Index of the script itself in the module
    pub const MAIN: u32 = 0;
}

impl fmt::Display for Reg {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "r{}", self.0)
    }
}

impl fmt::Display for Const {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "#{}", self.0)
    }
}

impl fmt::Display for Label {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "@{}", self.0)
    }
}
