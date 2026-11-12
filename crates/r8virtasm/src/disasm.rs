//! 反汇编：`Module` ⇒ 文本，信息完整（`FromStr for Module` 读回来逐项相同，见 asm.rs）。
//!
//! ```text
//! fn#0 "main" normal sloppy params 0 length 0 regs 3
//!   .mapped 2, -, 3
//!   #0 = string "x"
//!   scope#0 = ["x" 0 const tdz] []
//!   @0    lda.const #0
//! ```
use core::fmt;

use crate::{Constant, EvalName, EvalScope, Function, FunctionKind, Mode, Module};

/// 函数种类的写法
pub(crate) const KINDS: [(FunctionKind, &str); 5] = [
    (FunctionKind::Normal, "normal"),
    (FunctionKind::Arrow, "arrow"),
    (FunctionKind::Method, "method"),
    (FunctionKind::Constructor { derived: false }, "constructor"),
    (FunctionKind::Constructor { derived: true }, "constructor.derived"),
];

/// 模式的写法
pub(crate) const MODES: [(Mode, &str); 2] = [(Mode::Sloppy, "sloppy"), (Mode::Strict, "strict")];

/// 按（值, 名字）表写出
macro_rules! by_table {
    ($($ty:ty = $table:expr),*) => {$(
        impl fmt::Display for $ty {
            fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
                f.write_str($table.into_iter().find(|(v, _)| v == self).map_or("?", |(_, name)| name))
            }
        }
    )*};
}
by_table!(FunctionKind = KINDS, Mode = MODES);

impl fmt::Display for Constant {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Constant::Number(x) if x.is_nan() && x.to_bits() != f64::NAN.to_bits() => write!(f, "number nan:{:#x}", x.to_bits()),
            Constant::Number(x) => write!(f, "number {x:?}"),
            Constant::String(s) => write!(f, "string {s:?}"),
            Constant::BigInt(digits) => write!(f, "bigint {digits}"),
        }
    }
}

impl fmt::Display for EvalName {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{:?} {}", self.name, self.slot)?;
        for (on, word) in [(self.constant, " const"), (self.tdz, " tdz"), (self.self_name, " self_name")] {
            if on {
                f.write_str(word)?;
            }
        }
        Ok(())
    }
}

impl fmt::Display for EvalScope {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        for env in &self.0 {
            f.write_str(" [")?;
            for (i, name) in env.iter().enumerate() {
                write!(f, "{}{name}", if i == 0 { "" } else { ", " })?;
            }
            f.write_str("]")?;
        }
        Ok(())
    }
}

impl fmt::Display for Function {
    /// 函数头之后的各行（函数头要 `fn#i`，由 `Module` 写）
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{:?} {} {} params {} length {} regs {}", self.name, self.kind, self.mode, self.params, self.length, self.regs)?;
        for (on, word) in [(self.generator, " generator"), (self.asynchronous, " async"), (self.keeps_args, " keeps_args")] {
            if on {
                f.write_str(word)?;
            }
        }
        writeln!(f)?;
        if !self.mapped_params.is_empty() {
            f.write_str("  .mapped")?;
            for (i, slot) in self.mapped_params.iter().enumerate() {
                let sep = if i == 0 { " " } else { ", " };
                match slot {
                    Some(s) => write!(f, "{sep}{s}")?,
                    None => write!(f, "{sep}-")?,
                }
            }
            writeln!(f)?;
        }
        for (k, c) in self.consts.iter().enumerate() {
            writeln!(f, "  #{k} = {c}")?;
        }
        for (k, s) in self.scopes.iter().enumerate() {
            writeln!(f, "  scope#{k} ={s}")?;
        }
        for (pc, insn) in self.insns.iter().enumerate() {
            writeln!(f, "  @{pc:<4} {insn}")?;
        }
        Ok(())
    }
}

/// 反汇编：`module.to_string()`。
impl fmt::Display for Module {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        for (i, func) in self.functions.iter().enumerate() {
            write!(f, "fn#{i} {func}")?;
        }
        Ok(())
    }
}
