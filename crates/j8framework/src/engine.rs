use alloc::format;
use alloc::string::String;

use crate::Error;

#[derive(Clone, Copy, Debug)]
pub struct Source<'s> {
    pub text: &'s str,
    pub file: &'s str,
}

impl Source<'_> {
    pub fn at(&self, offset: u32) -> String {
        let before = &self.text[..(offset as usize).min(self.text.len())];
        let line = before.matches('\n').count() + 1;
        let col = before.rfind('\n').map_or(before.len(), |i| before.len() - i - 1) + 1;
        format!("{}:{line}:{col}", self.file)
    }

    pub fn unsupported(&self, offset: u32, what: &str) -> Error {
        Error::Unsupported(format!("{}: {what}", self.at(offset)))
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ScriptKind {
    Classic,
    Shared,
    Module,
}
