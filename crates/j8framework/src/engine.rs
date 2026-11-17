use alloc::format;
use alloc::string::String;

use crate::{ContextRoot, Error, Global, Isolate, Quota};

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

/// How a piece of source is run.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ScriptKind {
    /// A classic script: its completion value is the result
    Classic,
    /// A classic script many isolates run (a runtime's prelude): an engine with a code cache compiles it once and
    /// shares the result
    Shared,
    /// An ES module (loading, linking and evaluating what it imports); the result is `undefined`
    Module,
}

/// The unified entry point of an execution engine. Running is resumable: [`start`](Self::start) readies a piece of
/// source, [`resume`](Self::resume) runs it for a quota of steps at a time, so the event loop
/// ([`crate::EventLoop`]) can interleave event loops and host work; [`eval`](Self::eval) runs one to the end.
pub trait Engine {
    /// A started piece of source, paused between turns. Its execution state lives on the isolate's roots, so the
    /// job (like the isolate and the engine) may move to another thread between turns.
    type Job: Send;

    /// Steps per turn when [`eval`](Self::eval) runs a job to the end (the heap may collect between turns)
    const SLICE: usize = 10_000;

    /// Compiles `src` and readies it to run in context `cx` (nothing runs yet)
    fn start(&mut self, isolate: &mut Isolate<'_>, cx: &ContextRoot, src: &Source<'_>, kind: ScriptKind) -> Result<Self::Job, Error>;

    /// Runs `job` until it finishes, giving its value after draining microtasks, or until `quota` is spent, giving
    /// `None` with the job left to resume (the quota is then exhausted). A job that finished or failed must not be
    /// resumed again. Steps are what the engine counts (frame entries and backward jumps for j8executor).
    fn resume(&mut self, isolate: &mut Isolate<'_>, cx: &ContextRoot, job: &mut Self::Job, quota: &mut Quota) -> Result<Option<Global>, Error>;

    /// Runs `src` to completion in context `cx` (including draining microtasks), returning the value of the last
    /// expression statement, in turns of [`SLICE`](Self::SLICE) steps (collecting between them)
    fn eval(&mut self, isolate: &mut Isolate<'_>, cx: &ContextRoot, src: &Source<'_>) -> Result<Global, Error> {
        self.run_to_end(isolate, cx, src, ScriptKind::Classic)
    }

    /// Loads, links and evaluates `src` as a module (including the modules it imports, and draining microtasks)
    fn eval_module(&mut self, isolate: &mut Isolate<'_>, cx: &ContextRoot, src: &Source<'_>) -> Result<Global, Error> {
        self.run_to_end(isolate, cx, src, ScriptKind::Module)
    }

    /// [`start`](Self::start), then [`resume`](Self::resume) until it finishes
    fn run_to_end(&mut self, isolate: &mut Isolate<'_>, cx: &ContextRoot, src: &Source<'_>, kind: ScriptKind) -> Result<Global, Error> {
        let mut job = self.start(isolate, cx, src, kind)?;
        loop {
            if let Some(g) = self.resume(isolate, cx, &mut job, &mut Quota::from(Self::SLICE))? {
                return Ok(g);
            }
        }
    }
}
