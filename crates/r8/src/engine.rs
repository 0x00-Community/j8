use crate::{Context, Error, Value};

/// 执行层。
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum Tier {
    /// r8eval：AST 上直接求值
    Eval,
    /// mercedes：解释执行 r8bytecode 字节码
    Mercedes,
}

impl Tier {
    pub fn name(self) -> &'static str {
        match self {
            Tier::Eval => "r8eval",
            Tier::Mercedes => "mercedes",
        }
    }
}

/// 一段脚本：源码与文件名（只用于报错）。
#[derive(Clone, Copy, Debug)]
pub struct Source<'s> {
    pub text: &'s str,
    pub file: &'s str,
}

impl Source<'_> {
    /// 字节偏移 ⇒ `文件:行:列`。
    pub fn at(&self, offset: u32) -> String {
        let before = &self.text[..(offset as usize).min(self.text.len())];
        let line = before.matches('\n').count() + 1;
        let col = before.rfind('\n').map_or(before.len(), |i| before.len() - i - 1) + 1;
        format!("{}:{line}:{col}", self.file)
    }

    /// 这个执行层不支持 `offset` 处的写法。
    pub fn unsupported(&self, tier: Tier, offset: u32, what: &str) -> Error {
        Error::Unsupported { tier, what: format!("{}: {what}", self.at(offset)) }
    }
}

/// 各执行层的统一入口。
pub trait Engine {
    fn tier(&self) -> Tier;

    /// 在 `cx` 里跑完 `src`，返回最后一条表达式语句的值。
    fn eval(&mut self, cx: &mut Context<'_, '_>, src: &Source<'_>) -> Result<Value, Error>;
}
