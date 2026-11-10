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

/// 常量池里的一项。数按位比（`0` 与 `-0` 不合并、`NaN` 能合并）。
#[derive(Clone, Debug)]
pub enum Constant {
    Number(f64),
    String(Box<str>),
}

impl PartialEq for Constant {
    fn eq(&self, o: &Self) -> bool {
        match (self, o) {
            (Constant::Number(a), Constant::Number(b)) => a.to_bits() == b.to_bits(),
            (Constant::String(a), Constant::String(b)) => a == b,
            _ => false,
        }
    }
}

/// 常量池下标。
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct Const(pub u32);

/// 一条指令。累加器 `acc` 是隐式的输入 / 输出。
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Insn {
    LdaUndefined,
    LdaNull,
    LdaBool(bool),
    /// acc = 常量
    LdaConst(Const),
    /// acc = reg
    Ldar(Reg),
    /// reg = acc
    Star(Reg),
    /// acc = 全局对象的属性，没有就抛 ReferenceError
    LdaGlobal(Const),
    /// 同上，没有给 `undefined`（`typeof x`）
    LdaGlobalOrUndefined(Const),
    /// 全局对象的属性 = acc
    StaGlobal(Const),
    /// acc = 往外第 `depth` 层环境的 `slot`
    LdaEnv { depth: u16, slot: u16 },
    /// 往外第 `depth` 层环境的 `slot` = acc
    StaEnv { depth: u16, slot: u16 },
    /// 进一层带 `slots` 个槽的新环境
    PushEnv(u16),
    /// 退回外层环境
    PopEnv,
    /// 当前环境换成一份拷贝（`for (let …)` 每轮一份新绑定）
    CloneEnv,
    /// acc = reg op acc
    Binary(BinaryOp, Reg),
    /// acc = reg op acc（布尔）
    Compare(CompareOp, Reg),
    /// acc = op acc
    Unary(UnaryOp),
    /// acc = ToNumber(acc)
    ToNumber,
    /// acc = acc + 1（acc 已是数）
    Inc,
    /// acc = acc − 1（acc 已是数）
    Dec,
    Jump(Label),
    /// ToBoolean(acc) 为真就跳
    JumpIfTrue(Label),
    JumpIfFalse(Label),
    /// acc 不是 `undefined` / `null` 就跳（`??`）
    JumpIfNotNullish(Label),
    /// acc = 本模块第 `n` 个函数的闭包，捕获当前环境
    CreateClosure(u32),
    /// acc = {}
    CreateObject,
    /// acc = [first, first+1, …]（`count` 个）
    CreateArray { first: Reg, count: u16 },
    /// acc = obj.name
    GetNamed { obj: Reg, name: Const },
    /// obj.name = acc
    SetNamed { obj: Reg, name: Const },
    /// acc = obj[acc]
    GetKeyed { obj: Reg },
    /// obj[key] = acc
    SetKeyed { obj: Reg, key: Reg },
    /// acc = callee.call(this, args…)：`args` 起连续 `argc` 个寄存器
    Call { callee: Reg, this: Reg, args: Reg, argc: u16 },
    /// 装一个异常处理器：抛出时跳到 `Label`（acc 是异常值），环境退回到装的时候
    PushHandler(Label),
    /// 卸掉最近装的处理器
    PopHandler,
    /// 抛出 acc
    Throw,
    /// 返回 acc
    Return,
}

impl Insn {
    /// 跳转指令的目标（回填用）。
    pub fn target_mut(&mut self) -> Option<&mut Label> {
        match self {
            Insn::Jump(l) | Insn::JumpIfTrue(l) | Insn::JumpIfFalse(l) | Insn::JumpIfNotNullish(l) | Insn::PushHandler(l) => Some(l),
            _ => None,
        }
    }
}

/// 一个函数：名字、形参数、寄存器数、指令、常量池。
#[derive(Clone, Debug, Default)]
pub struct Function {
    pub name: Box<str>,
    pub params: u16,
    pub regs: u16,
    pub insns: Vec<Insn>,
    pub consts: Vec<Constant>,
}

impl Function {
    /// 常量池里的字符串（属性名 / 全局名）。
    pub fn name_at(&self, k: Const) -> &str {
        match &self.consts[k.0 as usize] {
            Constant::String(s) => s,
            Constant::Number(_) => unreachable!("a name constant is a string"),
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

impl fmt::Display for Insn {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match *self {
            Insn::LdaUndefined => f.write_str("lda.undefined"),
            Insn::LdaNull => f.write_str("lda.null"),
            Insn::LdaBool(b) => write!(f, "lda.bool {b}"),
            Insn::LdaConst(k) => write!(f, "lda.const #{}", k.0),
            Insn::Ldar(r) => write!(f, "ldar {r}"),
            Insn::Star(r) => write!(f, "star {r}"),
            Insn::LdaGlobal(k) => write!(f, "lda.global #{}", k.0),
            Insn::LdaGlobalOrUndefined(k) => write!(f, "lda.global.or_undefined #{}", k.0),
            Insn::StaGlobal(k) => write!(f, "sta.global #{}", k.0),
            Insn::LdaEnv { depth, slot } => write!(f, "lda.env {depth}.{slot}"),
            Insn::StaEnv { depth, slot } => write!(f, "sta.env {depth}.{slot}"),
            Insn::PushEnv(n) => write!(f, "push.env {n}"),
            Insn::PopEnv => f.write_str("pop.env"),
            Insn::CloneEnv => f.write_str("clone.env"),
            Insn::Binary(op, r) => write!(f, "{} {r}", op.name()),
            Insn::Compare(op, r) => write!(f, "{} {r}", op.name()),
            Insn::Unary(op) => f.write_str(op.name()),
            Insn::ToNumber => f.write_str("to_number"),
            Insn::Inc => f.write_str("inc"),
            Insn::Dec => f.write_str("dec"),
            Insn::Jump(l) => write!(f, "jump @{}", l.0),
            Insn::JumpIfTrue(l) => write!(f, "jump.if_true @{}", l.0),
            Insn::JumpIfFalse(l) => write!(f, "jump.if_false @{}", l.0),
            Insn::JumpIfNotNullish(l) => write!(f, "jump.if_not_nullish @{}", l.0),
            Insn::CreateClosure(n) => write!(f, "create.closure fn#{n}"),
            Insn::CreateObject => f.write_str("create.object"),
            Insn::CreateArray { first, count } => write!(f, "create.array {first}, {count}"),
            Insn::GetNamed { obj, name } => write!(f, "get.named {obj}, #{}", name.0),
            Insn::SetNamed { obj, name } => write!(f, "set.named {obj}, #{}", name.0),
            Insn::GetKeyed { obj } => write!(f, "get.keyed {obj}"),
            Insn::SetKeyed { obj, key } => write!(f, "set.keyed {obj}, {key}"),
            Insn::Call { callee, this, args, argc } => write!(f, "call {callee}, {this}, {args}, {argc}"),
            Insn::PushHandler(l) => write!(f, "push.handler @{}", l.0),
            Insn::PopHandler => f.write_str("pop.handler"),
            Insn::Throw => f.write_str("throw"),
            Insn::Return => f.write_str("return"),
        }
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
