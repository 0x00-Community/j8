//! The command line. One executable serves as both `dune` and `dunx`, told apart by the name it is started under
//! (`dunx` is a link to `dune`). Each folder under `cli/` is one level of subcommands: `run/` is `dune run`, `repl/` is
//! `dune repl`, `x/` is `dune x` (which `dunx` is).
mod repl;
mod run;
mod x;

use std::ffi::OsStr;
use std::path::{Path, PathBuf};
use std::process::ExitCode;

use clap::{Parser, Subcommand};

use crate::job::{Job, Layer};
use repl::Repl;
use run::Run;
use x::{Dunx, X};

/// The j8 JavaScript runtime
#[derive(Parser)]
#[command(name = "dune", version, override_usage = Dune::USAGE)]
pub struct Dune {
    #[command(subcommand)]
    command: Option<Command>,
    /// Run this source
    #[arg(short, long, conflicts_with_all = ["argv", "print"])]
    eval: Option<String>,
    /// Run this source and print the result
    #[arg(short, long, conflicts_with = "argv")]
    print: Option<String>,
    /// Enable the JIT: hot functions are compiled to x86-64 machine code (j8jit)
    #[arg(long, global = true)]
    jit: bool,
    /// Web-standard globals only (no `process`, `require` or `node:` modules); files run as ES modules
    #[arg(long, global = true)]
    web: bool,
    /// Script to run (same as `dune run`; `-` reads stdin), then the arguments passed to it (after the script path in
    /// `process.argv`)
    #[arg(trailing_var_arg = true, allow_hyphen_values = true, value_name = "FILE [ARGS]")]
    argv: Vec<String>,
}

#[derive(Subcommand)]
enum Command {
    Run(Run),
    Repl(Repl),
    /// Run a package's executable, installing the package first when needed (the same as `dunx`)
    X(X),
}

/// What a `dune` command line asks for
enum Ask {
    Job(Job),
    X(X),
}

impl TryFrom<Dune> for Ask {
    type Error = String;

    fn try_from(cli: Dune) -> Result<Self, String> {
        let mut argv = cli.argv.into_iter();
        match (cli.command, argv.next(), cli.eval, cli.print) {
            (Some(Command::X(x)), ..) => Ok(Ask::X(x)),
            (Some(Command::Run(run)), ..) => Job::try_from(run).map(Ask::Job),
            (None, Some(file), ..) => Job::try_from(Run { file: file.into(), args: argv.collect() }).map(Ask::Job),
            (Some(Command::Repl(repl)), ..) => Ok(Ask::Job(repl.into())),
            (None, None, None, None) => Ok(Ask::Job(Repl.into())),
            (None, None, Some(text), _) => Ok(Ask::Job(Job::Script { text, file: "[eval]".into(), print: false })),
            (None, None, None, Some(text)) => Ok(Ask::Job(Job::Script { text, file: "[eval]".into(), print: true })),
        }
    }
}

impl Dune {
    const USAGE: &str = "dune [OPTIONS] [FILE] [ARGS]...\n       dune [OPTIONS] <COMMAND>";

    fn run(self) -> ExitCode {
        let (layer, jit) = (if self.web { Layer::Web } else { Layer::Node }, self.jit);
        match Ask::try_from(self) {
            Ok(Ask::Job(job)) => job.run(layer, jit),
            Ok(Ask::X(x)) => x.run(jit),
            Err(e) => {
                eprintln!("dune: {e}");
                ExitCode::FAILURE
            }
        }
    }
}

/// The name this executable was started under
#[derive(Clone, Copy)]
enum Program {
    Dune,
    Dunx,
}

impl Program {
    const DUNX: &str = "dunx";
}

impl From<&Path> for Program {
    /// By the file name without its extension (`dunx`, `dunx.exe`); anything else is `dune`
    fn from(exe: &Path) -> Self {
        match exe.file_stem().and_then(OsStr::to_str) {
            Some(Self::DUNX) => Self::Dunx,
            _ => Self::Dune,
        }
    }
}

/// A parsed command line, of `dune` or of `dunx`
pub enum Cli {
    Dune(Dune),
    Dunx(Dunx),
}

impl Cli {
    /// Parses this process's command line as the program its name says
    pub fn parse() -> Self {
        let exe = std::env::args_os().next().map(PathBuf::from).unwrap_or_default();
        match Program::from(exe.as_path()) {
            Program::Dune => Self::Dune(Dune::parse()),
            Program::Dunx => Self::Dunx(Dunx::parse()),
        }
    }

    pub fn run(self) -> ExitCode {
        match self {
            Self::Dune(dune) => dune.run(),
            Self::Dunx(dunx) => dunx.run(),
        }
    }
}
