//! r8dbg：r8 的调试命令行。
mod asm;

use std::process::ExitCode;

use clap::{Parser, Subcommand};

use asm::{Asm, Disasm};

/// r8 调试工具
#[derive(Parser)]
#[command(name = "r8dbg", version)]
struct Cli {
    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand)]
enum Command {
    /// JS ⇒ 字节码，打印反汇编清单
    Disasm(Disasm),
    /// 汇编反汇编清单，打印规范化后的清单
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
            eprintln!("r8dbg: {e}");
            ExitCode::FAILURE
        }
    }
}
