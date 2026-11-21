mod code;

use std::process::ExitCode;

use clap::{Parser, Subcommand};

use code::Compiled;

#[derive(Parser)]
#[command(about = "j8 debugging tool")]
#[command(name = "j8bcdbg", version)]
struct Cli {
    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand)]
enum Command {
    #[command(about = "JS ⇒ bytecode, listed like V8's --print-bytecode")]
    Compile(Compiled),
}

impl Command {
    fn run(&self) -> Result<(), String> {
        match self {
            Command::Compile(c) => c.run(),
        }
    }
}

fn main() -> ExitCode {
    match Cli::parse().command.run() {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => {
            eprintln!("j8bcdbg: {e}");
            ExitCode::FAILURE
        }
    }
}
