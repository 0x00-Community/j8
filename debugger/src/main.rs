//! j8dbg: the j8 debugging CLI.
mod asm;

use std::process::ExitCode;

use clap::{Parser, Subcommand};

use asm::{Asm, Disasm};

/// j8 debugging tool
#[derive(Parser)]
#[command(name = "j8dbg", version)]
struct Cli {
    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand)]
enum Command {
    /// JS ⇒ bytecode, print the disassembly listing
    Disasm(Disasm),
    /// Assemble a disassembly listing, print the normalized listing
    Asm(Asm),
}

impl Command {
    fn run(&self) -> Result<(), String> {
        match self {
            Command::Disasm(d) => d.run(),
            Command::Asm(a) => a.run(),
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
