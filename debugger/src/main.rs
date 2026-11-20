//! j8dbg: the j8 debugging CLI.
mod code;

use std::process::ExitCode;

use clap::{Parser, Subcommand};

use code::Compiled;

/// j8 debugging tool
#[derive(Parser)]
#[command(name = "j8dbg", version)]
struct Cli {
    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand)]
enum Command {
    /// JS ⇒ bytecode, listed like V8's --print-bytecode
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
            eprintln!("j8dbg: {e}");
            ExitCode::FAILURE
        }
    }
}
