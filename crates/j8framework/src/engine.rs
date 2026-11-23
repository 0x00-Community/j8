use alloc::format;
use alloc::string::String;

use crate::{ContextRoot, Error, Global, Isolate, Quota};

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

pub trait Engine<'p> {
    type Job: Send;

    const SLICE: usize = 10_000;

    fn start(&mut self, isolate: &mut Isolate<'p>, cx: &ContextRoot<'p>, src: &Source<'_>, kind: ScriptKind) -> Result<Self::Job, Error>;

    fn resume(&mut self, isolate: &mut Isolate<'p>, cx: &ContextRoot<'p>, job: &mut Self::Job, quota: &mut Quota) -> Result<Option<Global<'p>>, Error>;

    fn eval(&mut self, isolate: &mut Isolate<'p>, cx: &ContextRoot<'p>, src: &Source<'_>) -> Result<Global<'p>, Error> {
        self.run_to_end(isolate, cx, src, ScriptKind::Classic)
    }

    fn eval_module(&mut self, isolate: &mut Isolate<'p>, cx: &ContextRoot<'p>, src: &Source<'_>) -> Result<Global<'p>, Error> {
        self.run_to_end(isolate, cx, src, ScriptKind::Module)
    }

    fn run_to_end(&mut self, isolate: &mut Isolate<'p>, cx: &ContextRoot<'p>, src: &Source<'_>, kind: ScriptKind) -> Result<Global<'p>, Error> {
        let mut job = self.start(isolate, cx, src, kind)?;
        loop {
            if let Some(g) = self.resume(isolate, cx, &mut job, &mut Quota::from(Self::SLICE))? {
                return Ok(g);
            }
        }
    }
}
