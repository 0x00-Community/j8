//! dune: the r8 JavaScript runtime, comparable to node / deno.
//!
//! `dune a.js` / `dune run a.js` runs a file, `dune -e '<js>'` runs source (`-p` also prints the result), no arguments
//! starts the REPL.
mod node;
mod repl;
mod session;

use std::path::PathBuf;
use std::process::ExitCode;

use clap::{Parser, Subcommand};
use r8framework::{CreateParams, Isolate, Source};
use r8platform::DefaultPlatform;

use session::{Outcome, Session};

/// All of the process's memory (the JS heap's pages and every Rust allocation) comes from r8alloc: one reserved
/// address range, lock-free, tied to no thread. (`#[global_allocator]` only accepts a `static`; it is a zero-sized
/// handle.)
#[global_allocator]
static ALLOC: r8alloc::R8Alloc = r8alloc::R8Alloc;

/// The r8 JavaScript runtime
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
    /// Enable the JIT: hot functions are compiled to x86-64 machine code (r8jit)
    #[arg(long)]
    jit: bool,
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
    /// Run a script file (as a CommonJS module; `require` / `module` / `__dirname` are available)
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
        Ok(Job::Main { file: file.display().to_string(), args })
    }
}

impl Job {
    /// Run; with `jit`, ordinary JS functions are compiled to machine code once hot.
    fn run(&self, jit: bool) -> ExitCode {
        let platform = DefaultPlatform::default();
        let mut isolate = Isolate::new(&platform, CreateParams::default());
        let (script, args) = match self {
            Job::Main { file, args } => (file.as_str(), args.as_slice()),
            _ => ("[eval]", &[][..]),
        };
        let mut session = Session::new(&mut isolate, script, args, jit);
        match self {
            Job::Repl => session.repl(),
            Job::Main { file, .. } => {
                let call = format!("__dune.runMain({})", Session::quote(file));
                match session.eval(&Source { text: &call, file }) {
                    Outcome::Value(_) => ExitCode::SUCCESS,
                    failed => failed.report(),
                }
            }
            Job::Script { text, file, print } => match session.eval(&Source { text, file }) {
                Outcome::Value(v) => {
                    if *print {
                        println!("{v}");
                    }
                    ExitCode::SUCCESS
                }
                failed => failed.report(),
            },
        }
    }
}

fn main() -> ExitCode {
    let cli = Cli::parse();
    let jit = cli.jit;
    match Job::try_from(cli) {
        Ok(job) => job.run(jit),
        Err(e) => {
            eprintln!("dune: {e}");
            ExitCode::FAILURE
        }
    }
}
