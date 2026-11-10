use alloc::format;
use alloc::string::String;

use crate::{ContextRoot, Error, Global, Isolate};

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
    /// 在执行环境 `cx` 里跑完 `src`（含排空微任务），返回最后一条表达式语句的值。
    /// 引擎自己分片进入 isolate，片与片之间回收。
    fn eval(&mut self, isolate: &mut Isolate<'_>, cx: &ContextRoot, src: &Source<'_>) -> Result<Global, Error>;
}
