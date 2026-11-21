//! `dune repl`: the interactive REPL (also what `dune` with no arguments starts).
use crate::job::Job;

/// Interactive REPL (the default with no arguments)
#[derive(clap::Args)]
pub struct Repl;

impl From<Repl> for Job {
    fn from(_: Repl) -> Self {
        Job::Repl
    }
}
