//! Assembly: text ⇒ `Module`, the inverse of `Display for Module` in disasm.rs.
//!
//! How an instruction is written is defined by insn.json (`asm` in `enums!` generates `FromStr for Insn`, from the
//! same source as `Display`); this file only handles the hand-written structures: operands, function headers, the
//! constant pool, and scope tables. A `;` starts a comment that runs to the end of the line.
use alloc::boxed::Box;
use alloc::string::String;
use alloc::vec::Vec;
use core::fmt;
use core::str::FromStr;

use r8framework::{BinaryOp, CompareOp, UnaryOp};

use crate::disasm::{KINDS, MODES};
use crate::{Const, Constant, EvalName, EvalScope, Function, FunctionKind, Label, Mode, Module, Reg};

/// An assembly error.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum AsmError {
    /// Unknown mnemonic
    Mnemonic(Box<str>),
    /// Malformed operand
    Operand(Box<str>),
    /// Missing operand
    Missing,
    /// Unexpected trailing content
    Extra(Box<str>),
    /// Expected (the first) but found (the second)
    Expected(&'static str, Box<str>),
    /// Index out of sequence: expected (the first) but got (the second)
    Index(usize, usize),
    /// The line (1-based) where the error occurred
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

/// The text form of an operand (or of a word in a function header) ⇒ value.
pub(crate) trait Operand: Sized {
    fn operand(text: &str) -> Result<Self, AsmError>;
}

/// An index with a fixed prefix: `r3` / `#3` / `@3`
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

/// Read via `FromStr`
macro_rules! parsed {
    ($($ty:ty),*) => {$(
        impl Operand for $ty {
            fn operand(text: &str) -> Result<Self, AsmError> {
                text.parse().map_err(|_| AsmError::Operand(text.into()))
            }
        }
    )*};
}
parsed!(bool, i32, u16, u32, usize);

/// Reverse lookup in `ALL` by `name()`
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

/// Reverse lookup in a (value, name) table
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
    /// As written by `{:?}` (`NaN` / `inf` / `-0.0` all accepted); a non-canonical NaN is `nan:0x…` (bitwise)
    fn operand(text: &str) -> Result<Self, AsmError> {
        match text.strip_prefix("nan:0x") {
            Some(hex) => u64::from_str_radix(hex, 16).map(f64::from_bits).map_err(|_| AsmError::Operand(text.into())),
            None => text.parse().map_err(|_| AsmError::Operand(text.into())),
        }
    }
}

/// The operands after an instruction mnemonic: comma-separated, taken in field order by the generated
/// `FromStr for Insn`.
pub(crate) struct Operands<'a>(Option<&'a str>);

impl<'a> Operands<'a> {
    /// Splits into mnemonic and operands
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

    /// The next operand, with the prefix `prefix` (`fn#` etc.) stripped
    pub(crate) fn next<T: Operand>(&mut self, prefix: &str) -> Result<T, AsmError> {
        let text = self.take().ok_or(AsmError::Missing)?;
        T::operand(text.strip_prefix(prefix).ok_or_else(|| AsmError::Operand(text.into()))?.trim_start())
    }

    /// Only present if it starts with `prefix` (an `Option` field, omitted entirely when `None`)
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

/// Lexer for one line: words, punctuation, strings with escapes.
struct Cursor<'a>(&'a str);

impl<'a> Cursor<'a> {
    /// Only whitespace or a comment remains
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

    /// Up to whitespace or `,` `[` `]` `;`; an empty string at end of line / before punctuation
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

    /// The index must be `want` (written in sequence)
    fn index(&mut self, want: usize) -> Result<(), AsmError> {
        match self.value::<usize>()? {
            got if got == want => Ok(()),
            got => Err(AsmError::Index(want, got)),
        }
    }

    /// A string as written by `{:?}`: `\n` `\r` `\t` `\0` `\\` `\"` `\'` `\u{…}`
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
    /// Function header `"name" kind mode params N length N regs N [generator] [async] [keeps_args]` (after `fn#i`)
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

    /// A line in the function body (after the header)
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
    /// One `[…]` per level, innermost first; within a level, comma-separated `"name" slot [const] [tdz] [self_name]`
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

/// Assembly: `text.parse::<Module>()`.
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
