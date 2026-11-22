use std::num::NonZeroUsize;
use std::path::Path;
use std::process::ExitCode;

use j8::Host;
use j8::executor::Executor;
use j8::frame::Source;
use j8::platform::DefaultPlatform;

use crate::node::{Dune, NodePlatform};
use crate::session::{Outcome, Session};
use crate::web::Web;

pub enum Job {
    Script { text: String, file: String, print: bool },
    Main { file: String, args: Vec<String> },
    Repl,
}

impl Job {
    pub fn main(file: &Path, args: Vec<String>) -> Result<Self, String> {
        if !file.is_file() {
            return Err(format!("{}: no such file", file.display()));
        }
        let file = dunce::canonicalize(file).map_err(|e| format!("{}: {e}", file.display()))?;
        Ok(Job::Main { file: file.display().to_string(), args })
    }
}

#[derive(Clone, Copy)]
pub enum Layer {
    Web,
    Node,
}

impl Layer {
    const fn entry(self) -> &'static str {
        match self {
            Self::Web => "runModule",
            Self::Node => "runMain",
        }
    }
}

impl Job {
    pub fn run(&self, layer: Layer, jit: bool) -> ExitCode {
        match layer {
            Layer::Web => self.run_on::<Web>(&Web::platform(DefaultPlatform::default()), layer, jit),
            Layer::Node => self.run_on::<Dune>(&Dune::platform(NodePlatform::from(DefaultPlatform::default())), layer, jit),
        }
    }

    fn run_on<H: Host>(&self, platform: &dyn j8::frame::Platform, layer: Layer, jit: bool) -> ExitCode {
        j8::EventLoopSet::scope(platform, NonZeroUsize::MIN, |set| self.run_in::<H>(set, layer, jit))
    }

    fn run_in<H: Host>(&self, set: &j8::EventLoopSet<'_, '_, Executor>, layer: Layer, jit: bool) -> ExitCode {
        let (script, args) = match self {
            Job::Main { file, args } => (Some(file.as_str()), args.as_slice()),
            _ => (None, &[][..]),
        };
        let mut session = Session::new::<H>(set, jit);
        if let Layer::Node = layer {
            session.node(script, args);
        }
        let outcome = match self {
            Job::Repl => return session.repl(),
            Job::Main { file, .. } => {
                let call = format!("__j8_native.{}({})", layer.entry(), Session::quote(file));
                session.eval(&Source { text: &call, file })
            }
            Job::Script { text, file, print } => {
                let outcome = session.eval(&Source { text, file });
                if let (Outcome::Value(v), true) = (&outcome, *print) {
                    println!("{v}");
                }
                outcome
            }
        };
        match outcome {
            Outcome::Value(_) => session.finish(),
            failed => failed.report(),
        }
    }
}
