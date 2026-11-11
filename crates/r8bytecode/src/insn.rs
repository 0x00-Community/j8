use alloc::boxed::Box;
use alloc::vec::Vec;
use core::fmt;

use r8frame::{BinaryOp, CompareOp, UnaryOp};

/// 寄存器：函数帧里的槽位。调用约定：`r0` 是 `this`，`r1` 是被调的函数本身，形参从 `r2` 起。
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct Reg(pub u16);

impl Reg {
    pub const THIS: Reg = Reg(0);
    pub const CALLEE: Reg = Reg(1);
    /// 第一个形参
    pub const PARAMS: u16 = 2;
}

/// 跳转目标：指令下标。
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct Label(pub u32);

r8macros::enums!("insn.json");

impl PartialEq for Constant {
    fn eq(&self, o: &Self) -> bool {
        match (self, o) {
            (Constant::Number(a), Constant::Number(b)) => a.to_bits() == b.to_bits(),
            (Constant::String(a), Constant::String(b)) | (Constant::BigInt(a), Constant::BigInt(b)) => a == b,
            _ => false,
        }
    }
}

/// 常量池下标。
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct Const(pub u32);

/// 代码的模式：非严格 / 严格。
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

/// 直接 eval 处看得见的一个绑定：名字、在那层环境里的槽位、是不是 `const`、要不要查死区。
#[derive(Clone, Debug)]
pub struct EvalName {
    pub name: Box<str>,
    pub slot: u16,
    pub constant: bool,
    pub tdz: bool,
    /// 具名函数表达式自己的名字：赋值非严格悄悄丢掉、严格抛 TypeError
    pub self_name: bool,
}

/// 直接 eval 处的作用域表：由内向外，一层对应环境链上的一个环境。
#[derive(Clone, Debug, Default)]
pub struct EvalScope(pub Vec<Vec<EvalName>>);

/// 一个函数：名字、形参数、寄存器数、指令、常量池。
#[derive(Clone, Debug, Default)]
pub struct Function {
    pub name: Box<str>,
    pub kind: FunctionKind,
    /// 进帧时把全部实参留在帧里（缺省派生构造器、剩余参数用）
    pub keeps_args: bool,
    /// 生成器函数：调用时不跑，返回生成器
    pub generator: bool,
    /// async 函数：调用时跑到第一个 `await`，返回 Promise
    pub asynchronous: bool,
    /// 严格模式：`this` 不换成全局对象、失败的赋值 / 删除抛错
    pub mode: Mode,
    pub params: u16,
    /// 函数的 `length`：第一个带默认值的形参（或剩余参数）之前的形参个数
    pub length: u16,
    pub regs: u16,
    pub insns: Vec<Insn>,
    pub consts: Vec<Constant>,
    /// 函数里各处直接 eval 的作用域表（`CallEval` 的 `scope` 是下标）
    pub scopes: Vec<EvalScope>,
}

impl Function {
    /// 常量池里的字符串（属性名 / 全局名）。
    pub fn name_at(&self, k: Const) -> &str {
        match &self.consts[k.0 as usize] {
            Constant::String(s) => s,
            Constant::Number(_) | Constant::BigInt(_) => unreachable!("a name constant is a string"),
        }
    }
}

/// 一个脚本编出的全部函数；第 0 个是脚本本身。
#[derive(Clone, Debug, Default)]
pub struct Module {
    pub functions: Vec<Function>,
}

impl Module {
    /// 脚本本身在模块里的下标
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

impl fmt::Display for Module {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        for (i, func) in self.functions.iter().enumerate() {
            writeln!(f, "fn#{i} {} (params {}, regs {}):", func.name, func.params, func.regs)?;
            for (k, c) in func.consts.iter().enumerate() {
                writeln!(f, "  #{k} = {c:?}")?;
            }
            for (pc, insn) in func.insns.iter().enumerate() {
                writeln!(f, "  @{pc:<4} {insn}")?;
            }
        }
        Ok(())
    }
}
