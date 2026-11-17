//! dune: a JavaScript runtime on j8, with two layers: [`web`] (the Web-standard APIs) and [`node`] (Node
//! compatibility on top of it).
//!
//! `dune a.js` / `dune run a.js` runs a file, `dune -e '<js>'` runs source (`-p` also prints the result), no arguments
//! starts the REPL; `--web` leaves the Node layer out.
mod host;
mod node;
mod repl;
mod session;
mod web;

use std::path::PathBuf;
use std::process::ExitCode;

use clap::{Parser, Subcommand};
use j8::Host;
use j8::frame::Source;
use j8::platform::DefaultPlatform;

use node::{Dune, NodePlatform};
use session::{Outcome, Session};
use web::Web;

/// The j8 JavaScript runtime
#[derive(Parser)]
#[command(name = "dune", version, args_conflicts_with_subcommands = true)]
struct Cli {
    #[command(subcommand)]
    command: Option<Command>,
    /// Script to run (same as `dune run`); `-` reads stdin
    file: Option<PathBuf>,
    /// Run this source
    #[arg(short, long, conflicts_with_all = ["file", "print"])]
    eval: Option<String>,
    /// Run this source and print the result
    #[arg(short, long, conflicts_with = "file")]
    print: Option<String>,
    /// Enable the JIT: hot functions are compiled to x86-64 machine code (j8jit)
    #[arg(long)]
    jit: bool,
    /// Web-standard globals only (no `process`, `require` or `node:` modules); files run as ES modules
    #[arg(long)]
    web: bool,
    /// Arguments passed to the script (after the script path in `process.argv`)
    #[arg(trailing_var_arg = true, allow_hyphen_values = true)]
    args: Vec<String>,
}

#[derive(Subcommand)]
enum Command {
    /// Run a script file
    Run {
        /// Script; `-` reads stdin
        file: PathBuf,
        /// Arguments passed to the script
        #[arg(trailing_var_arg = true, allow_hyphen_values = true)]
        args: Vec<String>,
    },
    /// Interactive REPL (the default with no arguments)
    Repl,
}

/// What to do this time.
enum Job {
    /// Run source; print the result when `print` is set
    Script { text: String, file: String, print: bool },
    /// Run a script file: CommonJS, or an ES module (`.mjs`, `"type": "module"`, always with `--web`)
    Main { file: String, args: Vec<String> },
    Repl,
}

impl TryFrom<Cli> for Job {
    type Error = String;

    fn try_from(cli: Cli) -> Result<Self, String> {
        let (file, args) = match (cli.command, cli.file, cli.eval, cli.print) {
            (Some(Command::Run { file, args }), ..) => (file, args),
            (None, Some(file), ..) => (file, cli.args),
            (Some(Command::Repl), ..) | (None, None, None, None) => return Ok(Job::Repl),
            (None, None, Some(text), _) => return Ok(Job::Script { text, file: "[eval]".into(), print: false }),
            (None, None, None, Some(text)) => return Ok(Job::Script { text, file: "[eval]".into(), print: true }),
        };
        if file.as_os_str() == "-" {
            let text = std::io::read_to_string(std::io::stdin()).map_err(|e| format!("[stdin]: {e}"))?;
            return Ok(Job::Script { text, file: "[stdin]".into(), print: false });
        }
        if !file.is_file() {
            return Err(format!("{}: no such file", file.display()));
        }
        let file = std::fs::canonicalize(&file).map_err(|e| format!("{}: {e}", file.display()))?;
        Ok(Job::Main { file: file.display().to_string(), args })
    }
}

/// Which runtime runs the job
#[derive(Clone, Copy)]
enum Layer {
    /// The Web-standard APIs only
    Web,
    /// Node compatibility on top of the Web layer
    Node,
}

impl Layer {
    /// The prelude's function that runs an entry file
    const fn entry(self) -> &'static str {
        match self {
            Self::Web => "runModule",
            Self::Node => "runMain",
        }
    }
}

impl Job {
    /// Run on `layer`; with `jit`, hot functions are compiled to machine code
    fn run(&self, layer: Layer, jit: bool) -> ExitCode {
        match layer {
            Layer::Web => self.run_on::<Web>(&Web::platform(DefaultPlatform::default()), layer, jit),
            Layer::Node => self.run_on::<Dune>(&Dune::platform(NodePlatform::from(DefaultPlatform::default())), layer, jit),
        }
    }

    fn run_on<H: Host>(&self, platform: &dyn j8::frame::Platform, layer: Layer, jit: bool) -> ExitCode {
        let (script, args) = match self {
            Job::Main { file, args } => (Some(file.as_str()), args.as_slice()),
            _ => (None, &[][..]),
        };
        let mut session = Session::new::<H>(platform, jit);
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

fn main() -> ExitCode {
    let cli = Cli::parse();
    let (layer, jit) = (if cli.web { Layer::Web } else { Layer::Node }, cli.jit);
    match Job::try_from(cli) {
        Ok(job) => job.run(layer, jit),
        Err(e) => {
            eprintln!("dune: {e}");
            ExitCode::FAILURE
        }
    }
}
