use std::process::ExitCode;

use j8::executor::Executor;
use j8::frame::{Error, Platform, Source};
use j8::{Host, Runtime};

use crate::node::Dune;

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
    /// A runtime of host `H` (its platform should be `H::platform(…)`, so `import` finds its modules)
    pub fn new<H: Host>(platform: &'p dyn Platform, jit: bool) -> Self {
        let executor = if jit { Executor::default().with_tier(std::rc::Rc::new(j8::jit::Jit::default())) } else { Executor::default() };
        match Runtime::new::<H>(platform, executor) {
            Ok(rt) => Self { rt },
            Err(e) => {
                eprintln!("dune: prelude failed: {e}");
                std::process::exit(1);
            }
        }
    }

    /// This run's `process.argv` and the rest of what depends on it (Node layer only)
    pub fn node(&mut self, script: Option<&str>, args: &[String]) {
        if let Err(e) = Dune::argv(&mut self.rt, script, args) {
            eprintln!("dune: setting up process failed: {e}");
            std::process::exit(1);
        }
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

    /// Runs `src`, then the event loop until no timer, I/O or other host work is left
    pub fn eval(&mut self, src: &Source<'_>) -> Outcome {
        match self.rt.eval(src.text, src.file).and_then(|g| self.rt.run_until_idle().map(|()| g)) {
            Ok(g) => Outcome::Value(self.rt.text(&g)),
            Err(Error::Exception(g)) => Outcome::Thrown(self.rt.text(&g)),
            Err(e) => Outcome::Failed(e.to_string()),
        }
    }

    /// The end of a run: `process` emits 'exit'; the exit code is `process.exitCode`
    pub fn finish(&mut self) -> ExitCode {
        match self.eval(&Source { text: "__j8_native.finish()", file: "[exit]" }) {
            Outcome::Value(code) => code.parse::<u8>().map_or(ExitCode::FAILURE, ExitCode::from),
            failed => failed.report(),
        }
    }
}
