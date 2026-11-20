//! `dune`: run a script file (`dune a.js` / `dune run a.js`), run source (`dune -e '<js>'`; `-p` also prints the
//! result), start the REPL (no arguments), or run a package's executable (`dune x <package>`, the same as `dunx`).
//! `--web` leaves the Node layer out.
use std::path::PathBuf;
use std::process::ExitCode;

use clap::{Parser, Subcommand};
use dune::{Job, Layer, X};

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
    /// Run a package's executable, installing the package first when needed (the same as `dunx`)
    X(X),
}

/// What this command line asks for
enum Ask {
    Job(Job),
    X(X),
}

impl TryFrom<Cli> for Ask {
    type Error = String;

    fn try_from(cli: Cli) -> Result<Self, String> {
        let (file, args) = match (cli.command, cli.file, cli.eval, cli.print) {
            (Some(Command::X(x)), ..) => return Ok(Ask::X(x)),
            (Some(Command::Run { file, args }), ..) => (file, args),
            (None, Some(file), ..) => (file, cli.args),
            (Some(Command::Repl), ..) | (None, None, None, None) => return Ok(Ask::Job(Job::Repl)),
            (None, None, Some(text), _) => return Ok(Ask::Job(Job::Script { text, file: "[eval]".into(), print: false })),
            (None, None, None, Some(text)) => return Ok(Ask::Job(Job::Script { text, file: "[eval]".into(), print: true })),
        };
        if file.as_os_str() == "-" {
            let text = std::io::read_to_string(std::io::stdin()).map_err(|e| format!("[stdin]: {e}"))?;
            return Ok(Ask::Job(Job::Script { text, file: "[stdin]".into(), print: false }));
        }
        Job::main(&file, args).map(Ask::Job)
    }
}

fn main() -> ExitCode {
    let cli = Cli::parse();
    let (layer, jit) = (if cli.web { Layer::Web } else { Layer::Node }, cli.jit);
    match Ask::try_from(cli) {
        Ok(Ask::Job(job)) => job.run(layer, jit),
        Ok(Ask::X(x)) => x.run(jit),
        Err(e) => {
            eprintln!("dune: {e}");
            ExitCode::FAILURE
        }
    }
}
