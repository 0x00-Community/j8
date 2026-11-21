//! `dune`: run a script file (`dune a.js` / `dune run a.js`), run source (`dune -e '<js>'`; `-p` also prints the
//! result), start the REPL (no arguments), or run a package's executable (`dune x <package>`). Started as `dunx` (a
//! link to `dune`), it is `dune x`.
use std::process::ExitCode;

fn main() -> ExitCode {
    dune::Cli::parse().run()
}
