use std::process::ExitCode;

use j8::executor::Executor;
use j8::frame::{Error, Source};
use j8::{Host, Runtime};

use crate::node::Dune;

pub enum Outcome {
    Value(String),
    Thrown(String),
    Failed(String),
}

impl Outcome {
    pub fn report(&self) -> ExitCode {
        match self {
            Outcome::Value(_) => return ExitCode::SUCCESS,
            Outcome::Thrown(e) => eprintln!("Uncaught {e}"),
            Outcome::Failed(e) => eprintln!("{e}"),
        }
        ExitCode::FAILURE
    }
}

pub struct Session<'p> {
    rt: Runtime<'p>,
}

impl<'p> Session<'p> {
    pub fn new<H: Host>(set: &j8::EventLoopSet<'_, 'p, Executor>, jit: bool) -> Self {
        #[cfg(all(unix, target_arch = "x86_64"))]
        let executor = if jit { Executor::default().with_tier(j8::jit::Jit::default()) } else { Executor::default() };
        #[cfg(not(all(unix, target_arch = "x86_64")))]
        let executor = {
            if jit {
                eprintln!("dune: --jit needs x86-64 on Linux or macOS; running without it");
            }
            Executor::default()
        };
        match Runtime::on::<H>(set, executor) {
            Ok(rt) => Self { rt },
            Err(e) => {
                eprintln!("dune: prelude failed: {e}");
                std::process::exit(1);
            }
        }
    }

    pub fn node(&mut self, script: Option<&str>, args: &[String]) {
        if let Err(e) = Dune::argv(&mut self.rt, script, args) {
            eprintln!("dune: setting up process failed: {e}");
            std::process::exit(1);
        }
    }

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
        match self.rt.eval(src.text, src.file).and_then(|g| self.rt.run_until_idle().map(|()| g)) {
            Ok(g) => Outcome::Value(self.rt.text(&g)),
            Err(Error::Exception(g)) => Outcome::Thrown(self.rt.text(&g)),
            Err(e) => Outcome::Failed(e.to_string()),
        }
    }

    pub fn finish(&mut self) -> ExitCode {
        match self.eval(&Source { text: "__j8_native.finish()", file: "[exit]" }) {
            Outcome::Value(code) => code.parse::<u8>().map_or(ExitCode::FAILURE, ExitCode::from),
            failed => failed.report(),
        }
    }
}
