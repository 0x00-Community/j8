use std::process::ExitCode;

use j8::Runtime;
use j8executor::Executor;
use j8framework::{Error, Platform, Source};

use crate::node::{Dune, Node};

/// The result of running a piece of source, already converted to text via ToString.
pub enum Outcome {
    Value(String),
    /// Uncaught exception
    Thrown(String),
    /// Can't compile (syntax error / unsupported construct)
    Failed(String),
}

impl Outcome {
    /// Print a failed result to stderr and return the exit code.
    pub fn report(&self) -> ExitCode {
        match self {
            Outcome::Value(_) => return ExitCode::SUCCESS,
            Outcome::Thrown(e) => eprintln!("Uncaught {e}"),
            Outcome::Failed(e) => eprintln!("{e}"),
        }
        ExitCode::FAILURE
    }
}

/// An execution environment: multiple pieces of source share one global object (one per REPL line); the runtime
/// outlives multiple entries.
pub struct Session<'p> {
    rt: Runtime<'p>,
}

impl<'p> Session<'p> {
    /// New environment: the [`Dune`] runtime (built-ins, `process`, `fs` / `path`, prelude) plus this run's `argv`.
    pub fn new(platform: &'p dyn Platform, script: &str, args: &[String], jit: bool) -> Self {
        let executor = if jit { Executor::default().with_tier(std::rc::Rc::new(j8jit::Jit::default())) } else { Executor::default() };
        let mut rt = match Dune::runtime(platform, executor) {
            Ok(rt) => rt,
            Err(e) => {
                eprintln!("dune: prelude failed: {e}");
                std::process::exit(1);
            }
        };
        rt.enter(|a| Node::argv(a, script, args));
        Self { rt }
    }

    /// String literal (JSON syntax).
    pub fn quote(s: &str) -> String {
        let mut out = String::from("\"");
        for c in s.chars() {
            match c {
                '"' => out.push_str("\\\""),
                '\\' => out.push_str("\\\\"),
                '\n' => out.push_str("\\n"),
                c => out.push(c),
            }
        }
        out.push('"');
        out
    }

    pub fn eval(&mut self, src: &Source<'_>) -> Outcome {
        // Then the event loop: async host work settles its promises before the result is read
        match self.rt.eval(src.text, src.file).and_then(|g| self.rt.run_until_idle().map(|()| g)) {
            Ok(g) => Outcome::Value(self.rt.text(&g)),
            Err(Error::Exception(g)) => Outcome::Thrown(self.rt.text(&g)),
            Err(e) => Outcome::Failed(e.to_string()),
        }
    }
}
