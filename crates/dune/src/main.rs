//! dune：r8 的 JavaScript 运行时，对标 node / deno。
//!
//! `dune a.js` / `dune run a.js` 跑文件，`dune -e '<js>'` 跑源码（`-p` 再打印结果），不带参数进 REPL。
mod repl;
mod session;

use std::path::PathBuf;
use std::process::ExitCode;

use clap::{Parser, Subcommand};
use r8executor::Executor;
use r8frame::{CreateParams, Isolate, Source};
use r8platform::DefaultPlatform;

use session::{Outcome, Session};

/// r8 的 JavaScript 运行时
#[derive(Parser)]
#[command(name = "dune", version, args_conflicts_with_subcommands = true)]
struct Cli {
    #[command(subcommand)]
    command: Option<Command>,
    /// 要跑的脚本（同 `dune run`）；`-` 读标准输入
    file: Option<PathBuf>,
    /// 跑这段源码
    #[arg(short, long, conflicts_with_all = ["file", "print"])]
    eval: Option<String>,
    /// 跑这段源码并打印结果
    #[arg(short, long, conflicts_with = "file")]
    print: Option<String>,
}

#[derive(Subcommand)]
enum Command {
    /// 跑一个脚本文件
    Run {
        /// 脚本；`-` 读标准输入
        file: PathBuf,
    },
    /// 交互式 REPL（不带参数时的缺省）
    Repl,
}

/// 这次要做什么。
enum Job {
    /// 跑源码，`print` 时打印结果
    Script { text: String, file: String, print: bool },
    Repl,
}

impl TryFrom<Cli> for Job {
    type Error = String;

    fn try_from(cli: Cli) -> Result<Self, String> {
        let file = match (cli.command, cli.file, cli.eval, cli.print) {
            (Some(Command::Run { file }), ..) | (None, Some(file), ..) => file,
            (Some(Command::Repl), ..) | (None, None, None, None) => return Ok(Job::Repl),
            (None, None, Some(text), _) => return Ok(Job::Script { text, file: "[eval]".into(), print: false }),
            (None, None, None, Some(text)) => return Ok(Job::Script { text, file: "[eval]".into(), print: true }),
        };
        let text = if file.as_os_str() == "-" {
            std::io::read_to_string(std::io::stdin()).map_err(|e| format!("[stdin]: {e}"))?
        } else {
            std::fs::read_to_string(&file).map_err(|e| format!("{}: {e}", file.display()))?
        };
        Ok(Job::Script { text, file: file.display().to_string(), print: false })
    }
}

impl Job {
    fn run(&self) -> ExitCode {
        let platform = DefaultPlatform::default();
        let mut isolate = Isolate::new(&platform, CreateParams::default());
        let mut executor = Executor::default();
        isolate.enter(|scope| {
            let mut session = Session::new(scope, &mut executor);
            match self {
                Job::Repl => session.repl(),
                Job::Script { text, file, print } => match session.eval(&Source { text, file }) {
                    Outcome::Value(v) => {
                        if *print {
                            println!("{v}");
                        }
                        ExitCode::SUCCESS
                    }
                    failed => failed.report(),
                },
            }
        })
    }
}

fn main() -> ExitCode {
    match Job::try_from(Cli::parse()) {
        Ok(job) => job.run(),
        Err(e) => {
            eprintln!("dune: {e}");
            ExitCode::FAILURE
        }
    }
}
