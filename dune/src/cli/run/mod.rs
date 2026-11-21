use std::path::PathBuf;

use crate::job::Job;

#[derive(clap::Args)]
#[command(about = "Run a script file")]
pub struct Run {
    #[arg(help = "Script; `-` reads stdin")]
    pub file: PathBuf,
    #[arg(help = "Arguments passed to the script (after the script path in `process.argv`)")]
    #[arg(trailing_var_arg = true, allow_hyphen_values = true)]
    pub args: Vec<String>,
}

impl TryFrom<Run> for Job {
    type Error = String;

    fn try_from(run: Run) -> Result<Self, String> {
        if run.file.as_os_str() == "-" {
            let text = std::io::read_to_string(std::io::stdin()).map_err(|e| format!("[stdin]: {e}"))?;
            return Ok(Job::Script { text, file: "[stdin]".into(), print: false });
        }
        Job::main(&run.file, run.args)
    }
}
