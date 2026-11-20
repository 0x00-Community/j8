//! `dunx`: run a package's executable (`dunx cowsay hi`, `dunx -p typescript tsc --version`), installing the package
//! from the npm registry into dune's cache when it is not in the project's `node_modules` or the cache yet.
use std::process::ExitCode;

use clap::Parser;
use dune::X;

/// Run a package's executable with dune
#[derive(Parser)]
#[command(name = "dunx", version)]
struct Cli {
    /// Enable the JIT: hot functions are compiled to x86-64 machine code (j8jit)
    #[arg(long)]
    jit: bool,
    #[command(flatten)]
    x: X,
}

fn main() -> ExitCode {
    let cli = Cli::parse();
    cli.x.run(cli.jit)
}
