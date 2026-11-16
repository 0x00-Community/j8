use alloc::format;
use alloc::string::String;

use crate::{ContextRoot, Error, Global, Isolate};

/// A script: source and file name (used only for error reporting).
#[derive(Clone, Copy, Debug)]
pub struct Source<'s> {
    pub text: &'s str,
    pub file: &'s str,
}

impl Source<'_> {
    /// Byte offset ⇒ `file:line:column`.
    pub fn at(&self, offset: u32) -> String {
        let before = &self.text[..(offset as usize).min(self.text.len())];
        let line = before.matches('\n').count() + 1;
        let col = before.rfind('\n').map_or(before.len(), |i| before.len() - i - 1) + 1;
        format!("{}:{line}:{col}", self.file)
    }

    /// The syntax at `offset` is not supported.
    pub fn unsupported(&self, offset: u32, what: &str) -> Error {
        Error::Unsupported(format!("{}: {what}", self.at(offset)))
    }
}

/// The unified entry point of an execution engine.
pub trait Engine {
    /// Run `src` to completion in execution context `cx` (including draining microtasks), returning the value of the
    /// last expression statement. The engine enters the isolate in slices itself, collecting between slices.
    fn eval(&mut self, isolate: &mut Isolate<'_>, cx: &ContextRoot, src: &Source<'_>) -> Result<Global, Error>;

    /// Load, link, and evaluate `src` as a module (including the modules it imports, and draining microtasks).
    fn eval_module(&mut self, isolate: &mut Isolate<'_>, cx: &ContextRoot, src: &Source<'_>) -> Result<Global, Error> {
        let _ = (isolate, cx);
        Err(src.unsupported(0, "modules"))
    }
}
