use crate::{Context, Error, Value};

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

    /// 不支持 `offset` 处的写法。
    pub fn unsupported(&self, offset: u32, what: &str) -> Error {
        Error::Unsupported(format!("{}: {what}", self.at(offset)))
    }
}

/// 执行引擎的统一入口。
pub trait Engine {
    /// 在 `cx` 里跑完 `src`，返回最后一条表达式语句的值。
    fn eval(&mut self, cx: &mut Context<'_, '_>, src: &Source<'_>) -> Result<Value, Error>;
}
