use crate::job::Job;

#[derive(clap::Args)]
#[command(about = "Interactive REPL (the default with no arguments)")]
pub struct Repl;

impl From<Repl> for Job {
    fn from(_: Repl) -> Self {
        Job::Repl
    }
}
