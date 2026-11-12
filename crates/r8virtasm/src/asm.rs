//! 汇编：文本 ⇒ `Module`，是 disasm.rs 里 `Display for Module` 的逆。
//!
//! 一条指令的写法由 insn.json 定（`enums!` 的 `asm` 生成 `FromStr for Insn`，与 `Display` 同源），
//! 这里只管操作数、函数头、常量池、作用域表这些手写结构。`;` 起到行尾是注释。
use alloc::boxed::Box;
use alloc::string::String;
use alloc::vec::Vec;
use core::fmt;
use core::str::FromStr;

use r8framework::{BinaryOp, CompareOp, UnaryOp};

use crate::disasm::{KINDS, MODES};
use crate::{Const, Constant, EvalName, EvalScope, Function, FunctionKind, Label, Mode, Module, Reg};

/// 汇编出错。
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum AsmError {
    /// 不认识的助记符
    Mnemonic(Box<str>),
    /// 写错的操作数
    Operand(Box<str>),
    /// 少了操作数
    Missing,
    /// 多出来的内容
    Extra(Box<str>),
    /// 该是（第一个）却看到（第二个）
    Expected(&'static str, Box<str>),
    /// 编号不连续：该是（第一个）却写了（第二个）
    Index(usize, usize),
    /// 第几行（从 1 数）出的错
    Line(usize, Box<AsmError>),
}

impl fmt::Display for AsmError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            AsmError::Mnemonic(m) => write!(f, "unknown mnemonic `{m}`"),
            AsmError::Operand(o) => write!(f, "bad operand `{o}`"),
            AsmError::Missing => f.write_str("missing operand"),
            AsmError::Extra(e) => write!(f, "unexpected `{e}`"),
            AsmError::Expected(what, found) => write!(f, "expected {what}, found `{found}`"),
            AsmError::Index(want, got) => write!(f, "index {got} out of sequence, expected {want}"),
            AsmError::Line(n, e) => write!(f, "line {n}: {e}"),
        }
    }
}

impl core::error::Error for AsmError {}

/// 一个操作数（或函数头里的一个词）的文本形式 ⇒ 值。
pub(crate) trait Operand: Sized {
    fn operand(text: &str) -> Result<Self, AsmError>;
}

/// 带固定前缀的编号：`r3` / `#3` / `@3`
macro_rules! prefixed {
    ($($ty:ident $prefix:literal),*) => {$(
        impl Operand for $ty {
            fn operand(text: &str) -> Result<Self, AsmError> {
                text.strip_prefix($prefix).and_then(|n| n.parse().ok()).map($ty).ok_or_else(|| AsmError::Operand(text.into()))
            }
        }
    )*};
}
prefixed!(Reg "r", Const "#", Label "@");

/// 照 `FromStr` 读的
macro_rules! parsed {
    ($($ty:ty),*) => {$(
        impl Operand for $ty {
            fn operand(text: &str) -> Result<Self, AsmError> {
                text.parse().map_err(|_| AsmError::Operand(text.into()))
            }
        }
    )*};
}
parsed!(bool, u16, u32, usize);

/// 按 `name()` 反查 `ALL`
macro_rules! by_name {
    ($($ty:ty),*) => {$(
        impl Operand for $ty {
            fn operand(text: &str) -> Result<Self, AsmError> {
                <$ty>::ALL.into_iter().find(|op| op.name() == text).ok_or_else(|| AsmError::Operand(text.into()))
            }
        }
    )*};
}
by_name!(BinaryOp, CompareOp, UnaryOp);

/// 按（值, 名字）表反查
macro_rules! by_table {
    ($($ty:ty = $table:expr),*) => {$(
        impl Operand for $ty {
            fn operand(text: &str) -> Result<Self, AsmError> {
                $table.into_iter().find(|(_, name)| *name == text).map(|(v, _)| v).ok_or_else(|| AsmError::Operand(text.into()))
            }
        }
    )*};
}
by_table!(FunctionKind = KINDS, Mode = MODES);

impl Operand for f64 {
    /// `{:?}` 的写法（`NaN` / `inf` / `-0.0` 都认）；非规范 NaN 是 `nan:0x…`（按位）
    fn operand(text: &str) -> Result<Self, AsmError> {
        match text.strip_prefix("nan:0x") {
            Some(hex) => u64::from_str_radix(hex, 16).map(f64::from_bits).map_err(|_| AsmError::Operand(text.into())),
            None => text.parse().map_err(|_| AsmError::Operand(text.into())),
        }
    }
}

/// 一条指令助记符后面的操作数：逗号分隔，生成的 `FromStr for Insn` 按字段次序取。
pub(crate) struct Operands<'a>(Option<&'a str>);

impl<'a> Operands<'a> {
    /// 拆成助记符与操作数
    pub(crate) fn split(text: &'a str) -> (&'a str, Self) {
        let text = text.trim();
        match text.split_once(char::is_whitespace) {
            Some((head, rest)) => (head, Self(Some(rest))),
            None => (text, Self(None)),
        }
    }

    fn peek(&self) -> Option<&'a str> {
        self.0.map(|r| r.split_once(',').map_or(r, |(a, _)| a).trim())
    }

    fn take(&mut self) -> Option<&'a str> {
        let r = self.0?;
        let (a, rest) = r.split_once(',').map_or((r, None), |(a, b)| (a, Some(b)));
        self.0 = rest;
        Some(a.trim())
    }

    /// 下一个操作数，去掉前缀 `prefix`（`fn#` 等）
    pub(crate) fn next<T: Operand>(&mut self, prefix: &str) -> Result<T, AsmError> {
        let text = self.take().ok_or(AsmError::Missing)?;
        T::operand(text.strip_prefix(prefix).ok_or_else(|| AsmError::Operand(text.into()))?.trim_start())
    }

    /// 以 `prefix` 开头才是它（`Option` 字段，`None` 时整个不写）
    pub(crate) fn optional<T: Operand>(&mut self, prefix: &str) -> Result<Option<T>, AsmError> {
        match self.peek() {
            Some(text) if text.starts_with(prefix) => self.next(prefix).map(Some),
            _ => Ok(None),
        }
    }

    pub(crate) fn finish(self) -> Result<(), AsmError> {
        match self.0 {
            None => Ok(()),
            Some(rest) => Err(AsmError::Extra(rest.into())),
        }
    }
}

/// 一行的词法：词、标点、带转义的字符串。
struct Cursor<'a>(&'a str);

impl<'a> Cursor<'a> {
    /// 只剩空白或注释
    fn at_end(&mut self) -> bool {
        self.0 = self.0.trim_start();
        self.0.is_empty() || self.0.starts_with(';')
    }

    fn end(&mut self) -> Result<(), AsmError> {
        if self.at_end() { Ok(()) } else { Err(AsmError::Extra(self.0.into())) }
    }

    fn eat(&mut self, lit: &str) -> bool {
        self.0 = self.0.trim_start();
        self.0.strip_prefix(lit).inspect(|r| self.0 = r).is_some()
    }

    fn expect(&mut self, lit: &'static str) -> Result<(), AsmError> {
        if self.eat(lit) { Ok(()) } else { Err(AsmError::Expected(lit, self.0.into())) }
    }

    /// 到空白或 `,` `[` `]` `;` 为止；行尾 / 标点前给空串
    fn word(&mut self) -> &'a str {
        self.0 = self.0.trim_start();
        let end = self.0.find(|c: char| c.is_whitespace() || matches!(c, ',' | '[' | ']' | ';')).unwrap_or(self.0.len());
        let (w, rest) = self.0.split_at(end);
        self.0 = rest;
        w
    }

    fn value<T: Operand>(&mut self) -> Result<T, AsmError> {
        T::operand(self.word())
    }

    /// 编号必须是 `want`（顺序写出来的）
    fn index(&mut self, want: usize) -> Result<(), AsmError> {
        match self.value::<usize>()? {
            got if got == want => Ok(()),
            got => Err(AsmError::Index(want, got)),
        }
    }

    /// `{:?}` 写出的字符串：`\n` `\r` `\t` `\0` `\\` `\"` `\'` `\u{…}`
    fn string(&mut self) -> Result<Box<str>, AsmError> {
        self.expect("\"")?;
        let mut out = String::new();
        let mut chars = self.0.char_indices();
        while let Some((i, c)) = chars.next() {
            out.push(match c {
                '"' => {
                    self.0 = &self.0[i + 1..];
                    return Ok(out.into());
                }
                '\\' => match chars.next().map(|(_, e)| e) {
                    Some('n') => '\n',
                    Some('r') => '\r',
                    Some('t') => '\t',
                    Some('0') => '\0',
                    Some(e @ ('\\' | '"' | '\'')) => e,
                    Some('u') if chars.next().is_some_and(|(_, b)| b == '{') => {
                        let hex: String = chars.by_ref().map(|(_, h)| h).take_while(|&h| h != '}').collect();
                        u32::from_str_radix(&hex, 16).ok().and_then(char::from_u32).ok_or_else(|| AsmError::Operand(hex.into()))?
                    }
                    _ => return Err(AsmError::Operand(self.0[i..].into())),
                },
                c => c,
            });
        }
        Err(AsmError::Expected("\"", self.0.into()))
    }
}

impl Function {
    /// 函数头 `"名字" 种类 模式 params N length N regs N [generator] [async] [keeps_args]`（`fn#i` 之后）
    fn header(c: &mut Cursor<'_>) -> Result<Self, AsmError> {
        let mut f = Function { name: c.string()?, kind: c.value()?, mode: c.value()?, ..Function::default() };
        c.expect("params")?;
        f.params = c.value()?;
        c.expect("length")?;
        f.length = c.value()?;
        c.expect("regs")?;
        f.regs = c.value()?;
        loop {
            match c.word() {
                "" => break,
                "generator" => f.generator = true,
                "async" => f.asynchronous = true,
                "keeps_args" => f.keeps_args = true,
                w => return Err(AsmError::Extra(w.into())),
            }
        }
        Ok(f)
    }

    /// 函数体里的一行（函数头之后）
    fn line(&mut self, c: &mut Cursor<'_>) -> Result<(), AsmError> {
        if c.eat(".mapped") {
            loop {
                self.mapped_params.push(match c.word() {
                    "-" => None,
                    w => Some(u16::operand(w)?),
                });
                if !c.eat(",") {
                    break;
                }
            }
        } else if c.eat("scope#") {
            c.index(self.scopes.len())?;
            c.expect("=")?;
            self.scopes.push(EvalScope::parse(c)?);
        } else if c.eat("#") {
            c.index(self.consts.len())?;
            c.expect("=")?;
            self.consts.push(Constant::parse(c)?);
        } else if c.eat("@") {
            c.index(self.insns.len())?;
            let text = c.0.split(';').next().unwrap_or_default();
            c.0 = &c.0[text.len()..];
            self.insns.push(text.parse()?);
        } else {
            return Err(AsmError::Expected("`@pc`, `#k`, `scope#k` or `.mapped`", c.0.into()));
        }
        c.end()
    }
}

impl Constant {
    /// `number 1.5` / `string "…"` / `bigint 123`
    fn parse(c: &mut Cursor<'_>) -> Result<Self, AsmError> {
        Ok(match c.word() {
            "number" => Constant::Number(c.value()?),
            "string" => Constant::String(c.string()?),
            "bigint" => Constant::BigInt(c.word().into()),
            w => return Err(AsmError::Expected("number, string or bigint", w.into())),
        })
    }
}

impl EvalScope {
    /// 由内向外每层一对 `[…]`，层内 `"名字" 槽位 [const] [tdz] [self_name]` 逗号分隔
    fn parse(c: &mut Cursor<'_>) -> Result<Self, AsmError> {
        let mut scope = EvalScope::default();
        while c.eat("[") {
            let mut env = Vec::new();
            while !c.eat("]") {
                if !env.is_empty() {
                    c.expect(",")?;
                }
                let mut name = EvalName { name: c.string()?, slot: c.value()?, constant: false, tdz: false, self_name: false };
                loop {
                    match c.word() {
                        "" => break,
                        "const" => name.constant = true,
                        "tdz" => name.tdz = true,
                        "self_name" => name.self_name = true,
                        w => return Err(AsmError::Extra(w.into())),
                    }
                }
                env.push(name);
            }
            scope.0.push(env);
        }
        Ok(scope)
    }
}

impl Module {
    fn line(&mut self, line: &str) -> Result<(), AsmError> {
        let mut c = Cursor(line);
        if c.at_end() {
            return Ok(());
        }
        if c.eat("fn#") {
            c.index(self.functions.len())?;
            self.functions.push(Function::header(&mut c)?);
            return c.end();
        }
        self.functions.last_mut().ok_or_else(|| AsmError::Expected("`fn#0`", line.into()))?.line(&mut c)
    }
}

/// 汇编：`text.parse::<Module>()`。
impl FromStr for Module {
    type Err = AsmError;

    fn from_str(text: &str) -> Result<Self, AsmError> {
        let mut module = Module::default();
        for (n, line) in text.lines().enumerate() {
            module.line(line).map_err(|e| AsmError::Line(n + 1, Box::new(e)))?;
        }
        Ok(module)
    }
}
