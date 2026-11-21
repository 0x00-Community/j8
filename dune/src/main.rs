use std::process::ExitCode;

fn main() -> ExitCode {
    dune::Cli::parse().run()
}
