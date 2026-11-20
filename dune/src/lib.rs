//! dune: a JavaScript runtime on j8, with two layers: the Web-standard APIs and Node compatibility on top of it.
//! Two executables share this library: `dune` runs scripts, `dunx` (also `dune x`) runs a package's executable,
//! installing the package from the npm registry when it is not there yet.
mod host;
mod job;
mod node;
mod repl;
mod session;
mod web;
mod x;

pub use job::{Job, Layer};
pub use x::X;
