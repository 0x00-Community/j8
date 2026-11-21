//! dune: a JavaScript runtime on j8, with two layers: the Web-standard APIs and Node compatibility on top of it.
//! One executable, `dune`, runs scripts; started as `dunx` (a link to it) or as `dune x`, it runs a package's
//! executable, installing the package from the npm registry when it is not there yet.
mod cli;
mod host;
mod job;
mod node;
mod repl;
mod session;
mod web;

pub use cli::Cli;
pub use job::{Job, Layer};
